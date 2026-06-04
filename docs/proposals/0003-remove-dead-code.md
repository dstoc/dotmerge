# Proposal: remove dead code and the unused `exact` field

## Motivation

Several `pub` items in `JjClient`, one helper in `util`, and one field on
`RevisionSummary` have no callers. Because they are `pub`, the compiler does not
warn about them, so they accumulate silently and widen the surface area that
future refactors (especially the single-session and typed-id proposals) have to
carry along.

## Problem statement

Confirmed dead items, with their only definition sites:

```text
util::not_implemented            src/util.rs:4     no callers
JjClient::read_tree              src/jj.rs:89      no callers
JjClient::file_at_rev            src/jj.rs:149     no callers
JjClient::read_entry_at_rev      src/jj.rs:111     only caller is file_at_rev
JjClient::set_bookmark           src/jj.rs:169     no callers
JjClient::clear_bookmark         src/jj.rs:179     no callers
JjClient::create_merge_change    src/jj.rs:560     pass-through wrapper
RevisionSummary::exact (field)   src/model.rs:7    written, never read
```

Notes on the riskier ones:

- `file_at_rev` (`src/jj.rs:149`) has untested, surprising behavior: it returns
  a symlink's target as bytes and errors on conflict entries. Keeping it invites
  a future caller to depend on that shape. Its only dependency,
  `read_entry_at_rev`, exists solely to back it.
- `set_bookmark` / `clear_bookmark` are superseded in practice: bookmark moves
  go through `complete_sync` (`src/jj.rs:188`) and the inline transactions in
  `create_or_refresh_import`.
- `create_merge_change` (`src/jj.rs:560`) just forwards to `create_new_change`
  (`src/jj.rs:523`); `merge_revisions` could call `create_new_change` directly.
- `RevisionSummary::exact` is set to `true`/`false` in the constructors
  (`src/model.rs:15,23`) but no code reads it.

## Proposal

Delete the items above. Specifically:

- Remove `util::not_implemented`.
- Remove `read_tree`, `file_at_rev`, and `read_entry_at_rev` from `JjClient`.
  `read_entries_at_rev` (`src/jj.rs:129`) remains and already covers the
  batch read path used by status, sync, and the clean check.
- Remove `set_bookmark` and `clear_bookmark`.
- Inline `create_merge_change` into its single caller in `merge_revisions`
  (`src/jj.rs:371`), or rename `create_new_change` to drop the wrapper.
- Remove the `exact` field from `RevisionSummary` and the two constructors that
  set it (`src/model.rs:10-25`).

### Guard against silent regrowth

Add `#![deny(dead_code)]` is too blunt for a library-shaped crate, but the crate
is a binary. Add `#![warn(unused)]` at the crate root in `src/main.rs` and,
because these were `pub`, downgrade items to `pub(crate)` or private wherever a
narrower visibility compiles. Once they are not `pub`, the existing dead-code
lint will catch future regressions automatically.

## Non-goals

- Not removing currently-unused-but-spec-relevant scaffolding that has a planned
  caller — only items with no design role. (`validate_resume_state` stays; see
  proposal 0002, which gives it a body.)
- Not reorganizing modules; that is proposal 0004.

## Verification

- `cargo build` and `cargo test` pass after removal.
- `cargo build` emits no new warnings, and emits dead-code warnings if any of
  the removed items are reintroduced without a caller (because visibility is now
  narrowed).
- `grep -rn` for each removed symbol returns only the deletion diff.

## Success criteria

- The seven items above are gone.
- No item in `JjClient` is `pub` unless it has an out-of-module caller.
- `RevisionSummary` has no field that is written but never read.
