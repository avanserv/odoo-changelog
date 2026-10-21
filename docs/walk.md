# Walk

`walk` records each version branch's first-parent history, and the commits its merges brought in,
and gives every commit the Odoo version it belongs to.

## Incremental walks

`walk` is incremental: it resumes each branch from the last recorded head and fails if a
branch was rewritten, or if a fork moved since the last walk (a new `[fork_overrides]` entry,
a changed `first_version`, a deepened clone), since either renumbers rows a resume does not
revisit. It also fails when a branch reaches a commit an earlier walk gave master, such as a
branch created from a commit master brought in through a merge: a full rebuild walks master
last and gives that commit to the branch. Rebuild the database in every case. Two runs on the
same refs produce identical databases, and an incremental walk equals a full rebuild.

## How versions are assigned

Each version branch's fork is where its first-parent chain meets master's. `git merge-base`
is not used: before 14.0 stable branches were merged forward into master, which moves the
merge-base to the last merged stable commit. When a shallow clone cuts the chain before the
fork, the community repo falls back to the parent of the master commit that bumped
`odoo/release.py` to the next series; `[fork_overrides]` in the config pins a fork by hand.

A commit on a version branch belongs to that version. A master commit belongs to the first
version forked after it (`version`) and to the X.0 release that includes it (`major`,
saas-17.2 → 18.0). Commits a merge brought in are recorded with the merge's `fp_idx` and a
`sub_idx` from 1, oldest first.
