//! Read-only access to an Odoo clone: version branches, fork points and commit walks.

mod forks;

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use anyhow::{Context, Result, anyhow};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::revision::walk::Sorting;
use odoo_changelog_core::Version;

pub use forks::{Fork, ForkSource, resolve_forks};

pub struct Repo {
    pub name: String,
    repo: gix::Repository,
    remote: String,
}

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub parents: Vec<ObjectId>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: i64,
    pub committed_at: i64,
    pub message: String,
}

/// One first-parent step of a branch and the commits its merge brought in, oldest first.
#[derive(Debug)]
pub struct Step {
    pub commit: ObjectId,
    pub merged: Vec<ObjectId>,
}

impl Repo {
    pub fn open(name: &str, path: &std::path::Path, remote: &str) -> Result<Self> {
        let mut repo = gix::open(path).with_context(|| format!("opening {}", path.display()))?;
        repo.object_cache_size_if_unset(64 * 1024 * 1024);
        Ok(Repo {
            name: name.into(),
            repo,
            remote: remote.into(),
        })
    }

    pub fn is_shallow(&self) -> Result<bool> {
        Ok(self.repo.is_shallow()?)
    }

    pub fn branch_head(&self, branch: &str) -> Result<Option<ObjectId>> {
        let name = format!("refs/remotes/{}/{}", self.remote, branch);
        match self.repo.try_find_reference(name.as_str())? {
            Some(mut r) => Ok(Some(r.peel_to_id()?.detach())),
            None => Ok(None),
        }
    }

    pub fn master_head(&self) -> Result<ObjectId> {
        self.branch_head("master")?
            .ok_or_else(|| anyhow!("{}: no {}/master", self.name, self.remote))
    }

    /// Remote version branches from `first` onwards, in release order.
    pub fn version_branches(&self, first: Version) -> Result<Vec<(Version, ObjectId)>> {
        let prefix = format!("refs/remotes/{}/", self.remote);
        let mut out = Vec::new();
        for r in self.repo.references()?.prefixed(prefix.as_str())? {
            let mut r = r.map_err(|e| anyhow!("{e}"))?;
            let short = r.name().as_bstr().to_str_lossy()[prefix.len()..].to_string();
            if let Some(v) = Version::parse_branch(&short).filter(|v| *v >= first) {
                out.push((v, r.peel_to_id()?.detach()));
            }
        }
        out.sort_by_key(|(v, _)| *v);
        Ok(out)
    }

    pub fn merge_base(&self, a: ObjectId, b: ObjectId) -> Result<Option<ObjectId>> {
        Ok(self.repo.merge_base(a, b)?.map(|id| id.detach()))
    }

    pub fn is_ancestor(&self, ancestor: ObjectId, of: ObjectId) -> Result<bool> {
        Ok(self.merge_base(ancestor, of)? == Some(ancestor))
    }

    /// First-parent chain from `tip` back to (excluding) `stop` and its ancestors, oldest first.
    pub fn first_parent_chain(&self, tip: ObjectId, stop: Option<ObjectId>) -> Result<Vec<ObjectId>> {
        let walk = self.repo.rev_walk([tip]).first_parent_only().with_hidden(stop);
        let mut ids = walk.all()?.map(|i| i.map(|i| i.id)).collect::<Result<Vec<_>, _>>()?;
        ids.reverse();
        Ok(ids)
    }

    /// The first commit on `tip`'s first-parent chain that is in `targets`.
    pub fn first_parent_meeting(
        &self,
        tip: ObjectId,
        targets: &std::collections::HashMap<ObjectId, usize>,
    ) -> Result<Option<ObjectId>> {
        for info in self.repo.rev_walk([tip]).first_parent_only().all()? {
            let id = info?.id;
            if targets.contains_key(&id) {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Commits a merge brought in that its first parent did not already contain, oldest first.
    pub fn merged_commits(&self, merge: ObjectId) -> Result<Vec<ObjectId>> {
        let commit = self.repo.find_commit(merge)?;
        let parents: Vec<ObjectId> = commit.parent_ids().map(|p| p.detach()).collect();
        if parents.len() < 2 {
            return Ok(Vec::new());
        }
        let walk = self
            .repo
            .rev_walk(parents[1..].to_vec())
            .sorting(Sorting::ByCommitTime(Default::default()))
            .with_hidden([parents[0]]);
        let infos = walk.all()?.collect::<Result<Vec<_>, _>>()?;

        // Kahn's sort, so a commit follows its parents even under clock skew; ties go to the
        // oldest commit time, then the sha. Parents outside the set are hidden or shallow.
        let mut pending: HashMap<ObjectId, usize> = HashMap::with_capacity(infos.len());
        let mut children: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
        let times: HashMap<ObjectId, i64> = infos.iter().map(|i| (i.id, i.commit_time())).collect();
        for info in &infos {
            let inside = info.parent_ids.iter().filter(|p| times.contains_key(*p));
            let mut count = 0;
            for parent in inside {
                children.entry(*parent).or_default().push(info.id);
                count += 1;
            }
            pending.insert(info.id, count);
        }
        let mut ready: BinaryHeap<Reverse<(i64, ObjectId)>> = pending
            .iter()
            .filter(|(_, n)| **n == 0)
            .map(|(id, _)| Reverse((times[id], *id)))
            .collect();
        let mut ids = Vec::with_capacity(infos.len());
        while let Some(Reverse((_, id))) = ready.pop() {
            ids.push(id);
            for child in children.get(&id).into_iter().flatten() {
                let n = pending.get_mut(child).expect("child is in the set");
                *n -= 1;
                if *n == 0 {
                    ready.push(Reverse((times[child], *child)));
                }
            }
        }
        Ok(ids)
    }

    pub fn walk_steps(&self, tip: ObjectId, stop: Option<ObjectId>) -> Result<Vec<Step>> {
        self.first_parent_chain(tip, stop)?
            .into_iter()
            .map(|commit| {
                Ok(Step {
                    commit,
                    merged: self.merged_commits(commit)?,
                })
            })
            .collect()
    }

    pub fn commit_info(&self, sha: ObjectId) -> Result<CommitInfo> {
        let c = self.repo.find_commit(sha)?;
        let author = c.author()?;
        Ok(CommitInfo {
            parents: c.parent_ids().map(|p| p.detach()).collect(),
            author_name: author.name.to_str_lossy().trim().to_string(),
            author_email: author.email.to_str_lossy().trim().to_string(),
            authored_at: author.time()?.seconds,
            committed_at: c.time()?.seconds,
            message: c.message_raw_sloppy().to_str_lossy().into_owned(),
        })
    }

    pub fn blob_id_at(&self, commit: ObjectId, path: &str) -> Result<Option<ObjectId>> {
        let tree = self.repo.find_commit(commit)?.tree()?;
        Ok(tree.lookup_entry_by_path(path)?.map(|e| e.object_id()))
    }

    pub fn read_blob(&self, oid: ObjectId) -> Result<Vec<u8>> {
        Ok(self.repo.find_blob(oid)?.take_data())
    }
}
