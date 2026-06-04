# Proposal: extract sync-phase policy out of the jj god-module

## Motivation

`src/jj.rs` is 767 lines and mixes layers: revset parsing, tree building,
bookmark I/O, conflict materialization, workspace loading, merge logic, and the
"disposable empty `@`" heuristics. The design doc planned distinct
`import.rs` / `merge.rs` / `export.rs` modules; instead that policy ended up
split between `sync.rs` and large methods on `JjClient`.

## Problem statement

The sync *policy* — what counts as a disposable placeholder, when to rewrite
`current-import` in place vs. replace it, when a merge can be skipped — is
tangled into the jj *plumbing* in three places:

```text
is_disposable_sync_placeholder              src/jj.rs:458
normalize_disposable_current_in_merge_inputs src/jj.rs:468
create_or_refresh_import (inline branch)    src/jj.rs:306-349
```

This area is also the project's bug magnet. Recent history:

```text
88d4a98 fix: avoid rewriting disposable empty @ during import
00dc045 fix: drop disposable empty @ from sync history
37eca31 fix: abandon redundant current-import state
f75fcb0 fix: reuse prepared state when target is already merged
2fadc34 fix: current-import is replaced, or becomes a child of @
```

Five of the recent commits touch exactly this logic. It is the highest-churn,
least-isolated code in the crate, and it has no unit tests of its own — it is
only exercised end-to-end through `tests/dotmerge_cli.rs`, which shells out to a
real `jj` binary and is slow to iterate on.

## Proposal

Split `JjClient` into a thin plumbing layer plus phase modules that hold policy.

### Plumbing stays in a `jj/` module

Move the mechanical jj-lib operations behind a narrow, side-effect-typed API in
`src/jj/mod.rs` (loading, revset resolution, tree reads, raw commit creation,
bookmark writes, ancestry, conflict queries). No policy decisions here.

### Phase policy moves to dedicated modules

```text
src/import.rs   create_or_refresh_import + disposable/normalize decisions
src/merge.rs    merge_revisions ancestor short-circuits + merge description
src/export.rs   export_revision_to_home (currently inline in sync.rs:67)
```

Each phase module takes the plumbing layer plus plain data and returns a
decision or a new revision. The disposable-placeholder predicate and the
"rewrite in place vs. replace vs. abandon" decision become pure-ish functions
that can be unit tested by constructing small fixture repos once.

`sync.rs` shrinks to orchestration:

```text
validate -> import::prepare -> merge::combine -> conflict gate
         -> export::to_home -> complete
```

### First extraction target

Start with the disposable-placeholder decision, because it is the buggiest and
most self-contained. Lift `is_disposable_sync_placeholder`,
`normalize_disposable_current_in_merge_inputs`, and the in-place/replace/abandon
branch of `create_or_refresh_import` into `import.rs`, expressed as a decision
enum:

```rust
enum ImportPlacement {
    ReuseRepoSide,                 // imported tree == @ tree
    RewriteInPlace(Commit),        // current-import already child of @
    ReplaceOnDisposableParent,     // @ is a disposable empty placeholder
    ReplaceOnCurrent,
}
```

Decide `ImportPlacement` first, unit test the decision against constructed
commit graphs, then apply it through the plumbing layer.

## Non-goals

- Not changing observable behavior; this is a structural refactor.
- Not introducing trait-based abstraction over jj-lib — concrete functions are
  enough for one backend.
- Not merging this with the single-session change (0001), though they compose:
  the phase functions should take the session from 0001 once it lands.

## Verification

- All existing `tests/dotmerge_cli.rs` cases pass unchanged.
- New fast unit tests in `import.rs` cover each `ImportPlacement` arm, including
  the regressions behind commits `88d4a98`, `00dc045`, and `37eca31`.
- `wc -l src/jj.rs` is materially smaller and contains no `dotmerge:`-prefixed
  description strings (those move to the phase modules).

## Success criteria

- `jj.rs` (or `jj/mod.rs`) contains no sync-policy branching.
- The disposable-placeholder decision is one named, unit-tested function.
- Import, merge, and export each live in their own module as the doc planned.
