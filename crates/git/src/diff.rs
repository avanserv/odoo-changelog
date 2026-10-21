//! Per-commit file changes for the extract pipeline, one `Worker` per thread.

use std::convert::Infallible;
use std::ops::ControlFlow;

use anyhow::{Result, anyhow};
use gix::ObjectId;
use gix::bstr::{BStr, ByteSlice};
use gix::diff::Rewrites;
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
        Ok(Some(out))
    }

    /// The `changes` of one commit as an extractor that `claims` paths sees them: its additions
    /// and deletions paired into renames as git does by default, sorted by path. Pairing only
    /// among the files one extractor claims keeps its events independent of the others.
    pub fn pair_renames(&mut self, changes: &[FileChange], claims: &dyn Fn(&str) -> bool) -> Result<Vec<FileChange>> {
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
        let mut tracker = gix::diff::rewrites::Tracker::new(REWRITES);
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
