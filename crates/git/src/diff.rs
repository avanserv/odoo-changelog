//! Per-commit file changes for the extract pipeline, one `Worker` per thread.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::ops::ControlFlow;

use anyhow::{Result, anyhow};
use gix::ObjectId;
use gix::bstr::{BStr, ByteSlice};
use gix::diff::Rewrites;
use gix::diff::blob::{Algorithm, Diff, InternedInput};
use gix::diff::tree::recorder::Change as Recorded;
use gix::diff::tree::visit::Change;
use gix::objs::TreeRefIter;
use gix::objs::tree::EntryKind;

use crate::Repo;

/// Git's defaults, pinned so that a clone's `diff.renames` cannot change what is paired.
const REWRITES: Rewrites = Rewrites {
    copies: None,
    percentage: Some(0.5),
    limit: 1000,
    track_empty: false,
};

/// Against a merge's second parent, a resolved file renamed on the first parent's side is an
/// addition that must pair with one of hundreds of deletions. gix compares sources × destinations
/// with `limit` unsquared, so this is the 1000 × 1000 git's default `diff.renameLimit` allows.
const MERGE_REWRITES: Rewrites = Rewrites {
    limit: 1_000_000,
    ..REWRITES
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSide {
    pub path: String,
    pub blob: ObjectId,
}

/// One file a commit changed: both sides for a modification or a paired rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub old: Option<FileSide>,
    pub new: Option<FileSide>,
}

impl FileChange {
    /// The new path, or the old one for a deletion.
    pub fn path(&self) -> &str {
        match (&self.new, &self.old) {
            (Some(side), _) | (None, Some(side)) => &side.path,
            (None, None) => unreachable!("a change has at least one side"),
        }
    }
}

/// What `Worker::merge_changes` found in one merge.
#[derive(Debug)]
pub struct MergeChanges {
    /// Per parent, in parent order: its changes at a resolved path, and its deletions when one
    /// of those is an addition, for `pair_merge` to pair with it.
    parents: Vec<Vec<FileChange>>,
    resolved: BTreeSet<String>,
}

/// A clone that any thread can open a `Worker` on.
pub struct SharedRepo(gix::ThreadSafeRepository);

pub struct Worker {
    repo: gix::Repository,
    state: gix::diff::tree::State,
    similarity: Option<gix::diff::blob::Platform>,
}

impl Repo {
    pub fn shared(&self) -> SharedRepo {
        SharedRepo(self.repo.clone().into_sync())
    }
}

impl SharedRepo {
    pub fn worker(&self) -> Worker {
        let mut repo = self.0.to_thread_local();
        // Every worker holds its own cache, so a quarter of what `Repo::open` gives the walk.
        repo.object_cache_size_if_unset(16 * 1024 * 1024);
        Worker {
            repo,
            state: Default::default(),
            similarity: None,
        }
    }
}

impl Worker {
    /// The blob files `commit` changed against its only parent, sorted by path, restricted to
    /// those whose path passes `keep`, which sees each path before any blob is read. A rename is
    /// a deletion and an addition here; `pair_renames` pairs them. `None` when the parent is
    /// missing from a shallow clone. A merge is an error.
    pub fn changed_files(&mut self, commit: ObjectId, keep: &dyn Fn(&str) -> bool) -> Result<Option<Vec<FileChange>>> {
        let (old_tree, new_tree) = {
            let c = self.repo.find_commit(commit)?;
            let parents: Vec<ObjectId> = c.parent_ids().map(|p| p.detach()).collect();
            let old_tree = match parents.as_slice() {
                [] => ObjectId::empty_tree(self.repo.object_hash()),
                [parent] => match self.repo.try_find_object(*parent)? {
                    Some(object) => object.try_into_commit()?.tree_id()?.detach(),
                    None => return Ok(None),
                },
                _ => anyhow::bail!("{commit} is a merge, which has no single parent to diff against"),
            };
            (old_tree, c.tree_id()?.detach())
        };

        self.diff_trees(old_tree, new_tree, keep).map(Some)
    }

    /// The files a merge left unlike every parent at one same place, as `git diff --cc` lists
    /// them, with each parent's changes at them; a file each side changed elsewhere and merged
    /// cleanly is not among them. `None` when a parent is missing from a shallow clone.
    /// `pair_merge` pairs them per extractor.
    pub fn merge_changes(&mut self, commit: ObjectId, keep: &dyn Fn(&str) -> bool) -> Result<Option<MergeChanges>> {
        let (parent_trees, new_tree) = {
            let c = self.repo.find_commit(commit)?;
            let mut trees = Vec::new();
            for parent in c.parent_ids() {
                match self.repo.try_find_object(parent.detach())? {
                    Some(object) => trees.push(object.try_into_commit()?.tree_id()?.detach()),
                    None => return Ok(None),
                }
            }
            (trees, c.tree_id()?.detach())
        };
        let mut parents = Vec::with_capacity(parent_trees.len());
        for tree in parent_trees {
            parents.push(self.diff_trees(tree, new_tree, keep)?);
        }
        let mut resolved: BTreeSet<String> = match parents.first() {
            Some(first) => first.iter().map(|c| c.path().to_string()).collect(),
            None => BTreeSet::new(),
        };
        for changes in parents.iter().skip(1) {
            let here: BTreeSet<&str> = changes.iter().map(FileChange::path).collect();
            resolved.retain(|p| here.contains(p.as_str()));
        }
        // A path added or deleted against a parent stays: the symbols decide a file renamed on
        // one side.
        let mut edits: BTreeMap<&str, (ObjectId, Vec<ObjectId>)> = BTreeMap::new();
        for changes in &parents {
            for c in changes {
                if let (Some(old), Some(new)) = (&c.old, &c.new)
                    && resolved.contains(&new.path)
                {
                    edits
                        .entry(&new.path)
                        .or_insert((new.blob, Vec::new()))
                        .1
                        .push(old.blob);
                }
            }
        }
        let mut clean = Vec::new();
        for (path, (merged, olds)) in edits {
            if olds.len() == parents.len() && !self.dense(merged, &olds)? {
                clean.push(path.to_string());
            }
        }
        for path in clean {
            resolved.remove(&path);
        }
        for changes in &mut parents {
            // A deletion outside `resolved` is kept only as a rename source for a resolved addition.
            let added = changes.iter().any(|c| c.old.is_none() && resolved.contains(c.path()));
            changes.retain(|c| resolved.contains(c.path()) || (added && c.new.is_none()));
        }
        Ok(Some(MergeChanges { parents, resolved }))
    }

    /// Each parent's `merge_changes` as an extractor that `claims` paths sees them: paired into
    /// renames as `pair_renames` does, then cut down to those that end at a resolved path.
    pub fn pair_merge(&mut self, merge: &MergeChanges, claims: &dyn Fn(&str) -> bool) -> Result<Vec<Vec<FileChange>>> {
        let mut out = Vec::with_capacity(merge.parents.len());
        for changes in &merge.parents {
            let mut paired = self.pair_with(changes, claims, MERGE_REWRITES)?;
            paired.retain(|c| merge.resolved.contains(c.path()));
            out.push(paired);
        }
        Ok(out)
    }

    /// Whether `merged` differs from each of `parents` at one same line, or gap between lines,
    /// as a hunk of `git diff --cc` does. A hunk marks the merged side's lines `i` as `2i + 1`
    /// and its bounding gaps as `2i`, so a deletion meets an edit next to it.
    fn dense(&self, merged: ObjectId, parents: &[ObjectId]) -> Result<bool> {
        let merged = self.repo.find_blob(merged)?;
        let mut common: Option<Vec<(u32, u32)>> = None;
        for &parent in parents {
            let parent = self.repo.find_blob(parent)?;
            let input = InternedInput::new(parent.data.as_slice(), merged.data.as_slice());
            let diff = Diff::compute(Algorithm::Myers, &input);
            let marks: Vec<(u32, u32)> = diff.hunks().map(|h| (2 * h.after.start, 2 * h.after.end)).collect();
            let marks = match common {
                Some(common) => overlap(&common, &marks),
                None => marks,
            };
            if marks.is_empty() {
                return Ok(false);
            }
            common = Some(marks);
        }
        Ok(common.is_some())
    }

    fn diff_trees(
        &mut self,
        old_tree: ObjectId,
        new_tree: ObjectId,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<FileChange>> {
        let mut recorder = gix::diff::tree::Recorder::default();
        {
            let old = self.repo.find_tree(old_tree)?;
            let new = self.repo.find_tree(new_tree)?;
            let kind = self.repo.object_hash();
            gix::diff::tree(
                TreeRefIter::from_bytes(&old.data, kind),
                TreeRefIter::from_bytes(&new.data, kind),
                &mut self.state,
                &self.repo.objects,
                &mut recorder,
            )?;
        }

        let mut out = Vec::new();
        for change in recorder.records {
            let (old, new, path) = match change {
                Recorded::Modification {
                    previous_entry_mode,
                    previous_oid,
                    entry_mode,
                    oid,
                    path,
                } if previous_oid != oid || previous_entry_mode.is_blob() != entry_mode.is_blob() => (
                    previous_entry_mode.is_blob().then_some(previous_oid),
                    entry_mode.is_blob().then_some(oid),
                    path,
                ),
                Recorded::Addition {
                    entry_mode, oid, path, ..
                } if entry_mode.is_blob() => (None, Some(oid), path),
                Recorded::Deletion {
                    entry_mode, oid, path, ..
                } if entry_mode.is_blob() => (Some(oid), None, path),
                _ => continue,
            };
            let path = lossy(path.as_ref());
            if (old.is_some() || new.is_some()) && keep(&path) {
                out.push(FileChange {
                    old: old.map(|oid| side(&path, oid)),
                    new: new.map(|oid| side(&path, oid)),
                });
            }
        }
        out.sort_by(by_path);
        Ok(out)
    }

    /// The `changes` of one commit as an extractor that `claims` paths sees them: its additions
    /// and deletions paired into renames as git does by default, sorted by path. Pairing only
    /// among the files one extractor claims keeps its events independent of the others.
    pub fn pair_renames(&mut self, changes: &[FileChange], claims: &dyn Fn(&str) -> bool) -> Result<Vec<FileChange>> {
        self.pair_with(changes, claims, REWRITES)
    }

    fn pair_with(
        &mut self,
        changes: &[FileChange],
        claims: &dyn Fn(&str) -> bool,
        rewrites: Rewrites,
    ) -> Result<Vec<FileChange>> {
        let mut out = Vec::new();
        let (mut added, mut deleted) = (Vec::new(), Vec::new());
        for change in changes {
            match (&change.old, &change.new) {
                (Some(_), Some(new)) if claims(&new.path) => out.push(change.clone()),
                (None, Some(new)) if claims(&new.path) => added.push(new),
                (Some(old), None) if claims(&old.path) => deleted.push(old),
                _ => {}
            }
        }
        if added.is_empty() || deleted.is_empty() {
            out.extend(added.into_iter().map(|new| FileChange {
                old: None,
                new: Some(new.clone()),
            }));
            out.extend(deleted.into_iter().map(|old| FileChange {
                old: Some(old.clone()),
                new: None,
            }));
            out.sort_by(by_path);
            return Ok(out);
        }

        // Executable or not, blobs pair with each other, so the mode the tracker sees is moot.
        let entry_mode = EntryKind::Blob.into();
        let mut tracker = gix::diff::rewrites::Tracker::new(rewrites);
        for new in added {
            let change = Change::Addition {
                entry_mode,
                oid: new.blob,
                relation: None,
            };
            tracker.try_push_change(change, new.path.as_bytes().as_bstr());
        }
        for old in deleted {
            let change = Change::Deletion {
                entry_mode,
                oid: old.blob,
                relation: None,
            };
            tracker.try_push_change(change, old.path.as_bytes().as_bstr());
        }
        let cache = match &mut self.similarity {
            Some(cache) => cache,
            None => self.similarity.insert(similarity_cache(&self.repo)?),
        };
        tracker
            .emit(
                |dest, source| {
                    let here = side(&lossy(dest.location), dest.change.oid().to_owned());
                    out.push(match (dest.change, source) {
                        (Change::Deletion { .. }, _) => FileChange {
                            old: Some(here),
                            new: None,
                        },
                        (_, Some(source)) => FileChange {
                            old: Some(side(&lossy(source.location), source.id)),
                            new: Some(here),
                        },
                        (_, None) => FileChange {
                            old: None,
                            new: Some(here),
                        },
                    });
                    ControlFlow::Continue(())
                },
                cache,
                &self.repo.objects,
                |_| Ok::<_, Infallible>(()),
            )
            .map_err(|e| anyhow!("{e}"))?;
        cache.clear_resource_cache_keep_allocation();
        out.sort_by(by_path);
        Ok(out)
    }

    /// Reads a blob from the object database; nothing here writes to the clone.
    pub fn read_blob(&self, oid: ObjectId) -> Result<Vec<u8>> {
        Ok(self.repo.find_blob(oid)?.take_data())
    }
}

/// `diff_resource_cache_for_tree_diff` would load `.git/index` and take attributes from
/// whatever the clone has checked out; an empty index keeps similarity a function of the blobs.
fn similarity_cache(repo: &gix::Repository) -> Result<gix::diff::blob::Platform> {
    let index = gix::index::State::new(repo.object_hash());
    let attributes = repo
        .attributes_only(&index, gix::worktree::stack::state::attributes::Source::IdMapping)?
        .detach();
    let mut cache = gix::diff::resource_cache(
        repo,
        gix::diff::blob::pipeline::Mode::ToGit,
        attributes,
        gix::diff::blob::pipeline::WorktreeRoots::default(),
    )?;
    cache.options.algorithm = Some(gix::diff::blob::Algorithm::Myers);
    Ok(cache)
}

fn by_path(a: &FileChange, b: &FileChange) -> std::cmp::Ordering {
    let old_path = |c: &FileChange| c.old.as_ref().map(|s| s.path.clone());
    a.path().cmp(b.path()).then_with(|| old_path(a).cmp(&old_path(b)))
}

fn side(path: &str, blob: ObjectId) -> FileSide {
    FileSide {
        path: path.to_string(),
        blob,
    }
}

fn lossy(path: &BStr) -> String {
    path.to_str_lossy().into_owned()
}

/// The overlaps of two sorted lists of disjoint inclusive ranges.
fn overlap(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        let (lo, hi) = (a[i].0.max(b[j].0), a[i].1.min(b[j].1));
        if lo <= hi {
            out.push((lo, hi));
        }
        if a[i].1 < b[j].1 { i += 1 } else { j += 1 }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlaps_keep_shared_points_and_drop_disjoint_ranges() {
        assert_eq!(
            overlap(&[(2, 4), (10, 12)], &[(4, 6), (7, 9), (11, 20)]),
            [(4, 4), (11, 12)]
        );
        assert_eq!(overlap(&[(2, 4)], &[(5, 8)]), []);
        assert_eq!(overlap(&[], &[(0, 9)]), []);
    }
}
