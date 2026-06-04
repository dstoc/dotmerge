# Proposal: implement resume-state validation for sync

## Motivation

The design doc treats `current-import` resume safety as a hard requirement. The
`sync` section says:

> if `current-import` exists, it must satisfy the resume preconditions reported
> by `status`; otherwise, error, report which check failed, and require manual
> repair; do not silently discard or rewrite `current-import`.

The conflict section repeats it: after a user resolves conflicts, rerunning
`dotmerge sync` should locate `current-import`, refresh it, and recompute the
merge — but only when the repo still "satisfies the sync resumability checks."

## Problem statement

`sync::run` calls `validate_resume_state` (`src/sync.rs:58`), but that function
is a stub:

```rust
fn validate_resume_state(
    _client: &JjClient,
    _base: &RevisionSummary,
    _target: &RevisionSummary,
    _current_import: Option<&RevisionSummary>,
) -> Result<()> {
    Ok(())
}
```

Every argument is discarded and it always returns `Ok`. So sync proceeds to
`create_or_refresh_import` unconditionally and rewrites `current-import` in
place regardless of its state. This is the one place where the implementation
silently does the thing the doc explicitly forbids ("do not silently discard or
rewrite `current-import`").

`status::collect` already computes the facts the check needs — whether
`current-import` matches `@`, whether it will be normalized to a child of `@`,
and whether conflicts are present (`src/status.rs:86-104`) — but sync does not
consume them.

## Proposal

Make `validate_resume_state` enforce the preconditions the doc and `status`
already describe, and run it before any mutation.

A `current-import` bookmark is resumable when all of the following hold:

```text
1. current-import resolves to exactly one commit (not conflicted)
2. current-import is base or a descendant of base
3. current-import's tree was reachable from a prior dotmerge import
   (its first parent is the current repo-side @ or @'s parent)
```

When `current-import` is absent, validation is trivially satisfied (fresh sync).

When it is present but fails a check, return a specific error naming the failed
check and pointing at manual repair, for example:

```text
current-import (kmnopqrs) is not a descendant of base (qpvuntsm)

it does not look like a dotmerge import on top of the current sync base.
inspect it with `jj log`, then either:
  - reset it with `jj bookmark delete current-import`
  - or move it onto the sync base before rerunning `dotmerge sync`
```

### Where the checks live

Factor the predicate into a single function that both `status` and `sync` call,
so the reported state and the enforced state cannot drift:

```rust
pub fn resume_state(
    client: &JjClient,
    base: &RevisionSummary,
    current_import: Option<&RevisionSummary>,
) -> Result<ResumeState>;

pub enum ResumeState {
    Fresh,                 // no current-import
    Resumable,             // safe to refresh in place
    Blocked { reason: String },
}
```

`status` renders `ResumeState` as a note. `sync` treats `Blocked` as a hard
error and refuses to touch `current-import`.

### Interaction with normalization

`create_or_refresh_import` already decides whether to rewrite `current-import`
in place or replace it (`src/jj.rs:308-356`). Validation runs first and only
gates the dangerous case: an existing `current-import` that is *not* a
recognizable dotmerge import. A `current-import` that is already a direct child
of `@`, or that needs normalization onto `@`, stays in the allowed set — that
is the normal resume path the doc describes.

## Non-goals

- Not adding auto-repair flags (`--reset-import`); the doc defers those.
- Not changing what counts as a clean working copy (separate check, already
  enforced at `src/sync.rs:15`).
- Not validating the target beyond the existing single-revision resolution.

## Verification

- Add a test where `current-import` points at an unrelated commit (not a
  descendant of base) and assert `dotmerge sync` errors without moving
  `current-import` or `last-sync`.
- Add a test where `current-import` is a normal interrupted import and assert
  sync resumes and completes, matching today's
  `sync_rerun_moves_current_import_after_repo_side_state_before_refresh`.
- Assert `dotmerge status` reports the same `Blocked`/`Resumable` verdict that
  `sync` enforces, using one shared code path.

## Success criteria

- `validate_resume_state` reads its arguments and can return `Err`.
- A non-dotmerge `current-import` blocks sync with a named reason.
- `status` and `sync` derive resumability from the same function.
