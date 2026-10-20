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
use gix::objs::tree::EntryMode;

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
    /// those whose old or new path passes `keep`, which sees each path before any blob is read.
    /// `None` when the parent is missing from a shallow clone. A merge is an error.
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
        let (mut added, mut deleted) = (Vec::new(), Vec::new());
        for change in recorder.records {
            match change {
                Recorded::Modification {
                    previous_entry_mode,
                    previous_oid,
                    entry_mode,
                    oid,
                    path,
                } => {
                    let path = path.to_str_lossy().into_owned();
                    if !keep(&path) {
                        continue;
                    }
                    match (previous_entry_mode.is_blob(), entry_mode.is_blob()) {
                        (true, true) if previous_oid != oid => out.push(FileChange {
                            old: Some(side(&path, previous_oid)),
                            new: Some(side(&path, oid)),
                        }),
                        (true, false) => deleted.push((path, previous_entry_mode, previous_oid)),
                        (false, true) => added.push((path, entry_mode, oid)),
                        _ => {}
                    }
                }
                Recorded::Addition {
                    entry_mode, oid, path, ..
                } if entry_mode.is_blob() => {
                    let path = path.to_str_lossy().into_owned();
                    if keep(&path) {
                        added.push((path, entry_mode, oid));
                    }
                }
                Recorded::Deletion {
                    entry_mode, oid, path, ..
                } if entry_mode.is_blob() => {
                    let path = path.to_str_lossy().into_owned();
                    if keep(&path) {
                        deleted.push((path, entry_mode, oid));
                    }
                }
                _ => {}
            }
        }

        if added.is_empty() || deleted.is_empty() {
            out.extend(added.into_iter().map(|(path, _, oid)| FileChange {
                old: None,
                new: Some(side(&path, oid)),
            }));
            out.extend(deleted.into_iter().map(|(path, _, oid)| FileChange {
                old: Some(side(&path, oid)),
                new: None,
            }));
        } else {
            self.pair_renames(added, deleted, &mut out)?;
        }
        out.sort_by(|a, b| {
            let old_path = |c: &FileChange| c.old.as_ref().map(|s| s.path.clone());
            (a.path(), old_path(a)).cmp(&(b.path(), old_path(b)))
        });
        Ok(Some(out))
    }

    /// Pairs additions with deletions as git does by default, reading only these blobs.
    fn pair_renames(
        &mut self,
        added: Vec<(String, EntryMode, ObjectId)>,
        deleted: Vec<(String, EntryMode, ObjectId)>,
        out: &mut Vec<FileChange>,
    ) -> Result<()> {
        let mut tracker = gix::diff::rewrites::Tracker::new(REWRITES);
        for (path, entry_mode, oid) in added {
            let change = Change::Addition {
                entry_mode,
                oid,
                relation: None,
            };
            tracker.try_push_change(change, path.as_bytes().as_bstr());
        }
        for (path, entry_mode, oid) in deleted {
            let change = Change::Deletion {
                entry_mode,
                oid,
                relation: None,
            };
            tracker.try_push_change(change, path.as_bytes().as_bstr());
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
        Ok(())
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

fn side(path: &str, blob: ObjectId) -> FileSide {
    FileSide {
        path: path.to_string(),
        blob,
    }
}

fn lossy(path: &BStr) -> String {
    path.to_str_lossy().into_owned()
}
