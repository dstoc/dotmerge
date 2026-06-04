# Proposal: single workspace session per command

## Motivation

`dotmerge` speaks to jj through `JjClient` in `src/jj.rs`. Almost every method
on that client begins by calling `load_workspace_and_repo()` (`src/jj.rs:568`),
which does a full `Workspace::load` followed by `repo_loader().load_at_head()`.

There are 17 call sites. A single `dotmerge sync` run hits this path roughly a
dozen times: `resolve_rev`, `is_working_copy_clean`, `create_or_refresh_import`,
`merge_revisions`, `current_revision`, `checkout_revision`, `has_conflicts`,
`complete_sync`, and then the trailing `status::collect`.

## Problem statement

Reloading the workspace per method has two costs.

First, consistency. Each mutating method opens its own transaction against a
separately loaded `load_at_head` snapshot and commits it independently:

```text
create_or_refresh_import -> tx.commit("refresh current-import")
checkout_revision        -> tx.commit("update working copy for dotmerge")
complete_sync            -> tx.commit("complete dotmerge sync")
```

There is no single consistent repo view across one logical sync. Between two
reloads the operation head can move (a concurrent `jj` command, or dotmerge's
own previous transaction), so a later step can reason about a different base
than an earlier step assumed.

Second, op-log noise. One `sync` produces several unrelated jj operations
instead of one grouped operation, which makes `jj op log` hard to read and hard
to undo as a unit.

The design doc already anticipated a session-shaped API: its sketched interface
uses `fn set_bookmark(&mut self, ...)`, implying one mutable handle rather than
`&self`-everywhere with per-call reloads.

## Proposal

Load the workspace and repo once per command and thread that state through the
operation.

Introduce a session type that owns the loaded workspace, the repo at head, and
an open transaction:

```rust
pub struct JjSession {
    workspace: Workspace,
    repo: Arc<ReadonlyRepo>,
    tx: Transaction,
    settings: UserSettings,
}
```

`JjClient::open` stays as the cheap handle that locates the workspace root.
Add `JjClient::begin(&self) -> Result<JjSession>` that performs the single
load and starts one transaction.

Read-only methods (`resolve_rev`, `list_files`, `read_entries_at_rev`,
`has_conflicts`, `bookmark_summary`, `is_ancestor`) move to `&JjSession` and
read from the already-loaded `repo`.

Mutating methods (`create_or_refresh_import`, `merge_revisions`,
`checkout_revision`, `complete_sync`) take `&mut JjSession` and stage their
changes on the session's single `tx` via `tx.repo_mut()` instead of opening and
committing their own transactions.

`sync::run` then becomes:

```text
let mut session = client.begin()?;
... import / merge / conflict check on session ...
session.checkout(&merged)?;          // staged, not committed
... export to $HOME (outside the tx) ...
session.complete_sync(&merged)?;     // staged
session.finish("dotmerge sync")?;    // one tx.commit
```

### Working-copy interaction

`checkout_revision` (`src/jj.rs:503`) currently commits a transaction and then
calls `workspace.check_out`. Keep the `check_out` filesystem step, but stage the
`edit` + `rebase_descendants` on the session transaction and run `check_out`
only after `finish`, using the committed op id. The working-copy write is the
one unavoidable side effect that must follow the commit.

### Export ordering

The critical invariant — `last-sync` only moves after a successful export — is
preserved by keeping `export_revision_to_home` between the merge and the
`complete_sync` staging, and only calling `finish` after export succeeds. If
export fails, the session is dropped without `finish`, so no jj state changes.

## Non-goals

- No change to the import/merge/export semantics themselves.
- No attempt to make export atomic across files (separate concern).
- Not introducing async; keep the existing `pollster::block_on` style.
- Not adding locking against concurrent external `jj` processes beyond what a
  single transaction already gives.

## Verification

- Run `dotmerge sync` on the existing integration fixtures and confirm
  `jj op log` shows one dotmerge operation per command instead of three.
- Existing tests in `tests/dotmerge_cli.rs` pass unchanged, in particular
  `sync_rerun_moves_current_import_after_repo_side_state_before_refresh` and
  `sync_reuses_target_without_preserving_disposable_empty_at`, which exercise
  multi-step state mutation.
- Add a test asserting that a sync interrupted before `finish` (simulated via an
  injected export failure) leaves `last-sync` and `current-import` unchanged.

## Success criteria

- One `load_at_head` per command, verified by instrumentation or review.
- One committed jj operation per mutating command.
- No method takes `&self` while mutating repo state.
