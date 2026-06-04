# Proposal: report what happened (sync) and what can be done (status)

## Motivation

`status` and `sync` share one renderer. `sync::run` finishes by calling
`status::collect_for_sync` and then `status::print_summary`
(`src/sync.rs:48-59`), so both commands print the identical block. That block is
written as a *forecast* — `next:` lists the steps a future sync *would* take. A
forecast is the right thing for `status`, but after `sync` has already imported,
merged, and exported, replaying "here is what sync would do" is misleading.

Captured output, immediately after a successful sync (`--target @`):

```
home:   matches base
target: differs from base (2 changes)
target changes since base:
  - modified .bashrc
  - added    .vimrc
next:
  - refresh `current-import` from the current managed `$HOME` state
  - merge the imported state with the requested target revision
  - export the merged files back to `$HOME` without deleting deletion candidates
```

Two problems are visible at once:

- **No confirmation.** Nothing reports that anything was imported, merged, or
  written, or that `last-sync` advanced. Success is inferred only from the
  absence of an error.
- **Stale forecast.** The target revision was pinned at sync start
  (`src/sync.rs:16`); after sync, `last-sync` moved past it, so re-comparing the
  two surfaces the pre-merge repo side as if it were pending work, and tells you
  to sync again.

Separately, the design doc frames `status` as the command that reports *whether
sync can run and what it will do*. Today's flat field list does not lead with a
verdict; the reader has to reconstruct the state from `home:` / `target:` lines,
the `notes:`, and the `next:` block.

## Problem statement

There are two coupled defects in one renderer:

1. **`sync` reports a forecast, not a result.** The post-sync recap should say
   what changed: was anything imported from `$HOME`, was a merge created (or a
   fast-forward, or nothing), and which files were actually written back —
   *only the files that differed from `$HOME`*, not every managed file.

2. **`status` buries its verdict.** It should lead with a single state — is the
   target ahead of base, is an import in progress, is a merge prepared but not
   yet exported, is there a conflict — and from that state name the one next
   move, instead of an unconditional three-line `next:` list.

A third, smaller defect blocks the recap:

3. **Export rewrites every managed file unconditionally.** `export_home_entries`
   (`src/fs.rs:190`) calls `write_atomic_file` / `write_atomic_symlink` for every
   entry with no comparison against the current on-disk content, and
   `export_revision_to_home` (`src/export.rs:8`) discards the `Vec<PathBuf>` it
   returns. So there is no "files that actually changed in `$HOME`" set to
   report, and every sync churns mtimes and re-`fsync`s files that did not move.

## Proposal

### 1. Phases return what they did

Each sync phase currently returns a bare `Revision`. Have them return a small
outcome the caller can render.

**Import.** `create_or_refresh_import` (`src/import.rs:25`) already computes
`base_tree_id`, `imported_tree_id`, and `current_tree_id`, and short-circuits to
`ImportPlacement::ReuseRepoSide` when `imported_tree_id == current_tree_id`
(`src/import.rs:203`) — i.e. `$HOME` matched the repo side and nothing was
imported. Return that fact plus the `base → $HOME` delta:

```rust
pub(crate) struct ImportOutcome {
    pub revision: Revision,
    pub imported: Vec<FileStatusSummary>, // empty ⇒ $HOME matched last-sync
}
```

The delta is the diff of `base` against the imported tree, classified with the
existing `classify_change` machinery (`src/status.rs:361`).

**Merge.** `merge_revisions` (`src/merge.rs:8`) already branches three ways.
Name them:

```rust
pub(crate) enum MergeOutcome {
    NoOp { revision: Revision },        // right ⊆ left: target already had the import
    FastForward { revision: Revision }, // left ⊆ right: advanced to target
    Merged { revision: Revision },      // create_new_change([left, right])
}
```

The three arms map directly onto the existing `is_ancestor` checks and the
`create_new_change` call (`src/merge.rs:23-31`).

**Export.** Make `export_home_entries` (`src/fs.rs:190`) diff-aware: for each
entry, read the current `$HOME` entry via the existing `read_rooted_entry`
helper and skip the write when it is byte-for-byte equal (same `ManagedEntry`).
Return the `Vec<FileStatusSummary>` of entries actually added or modified.
`export_revision_to_home` (`src/export.rs:8`) propagates that list instead of
discarding it.

### 2. `sync` prints a past-tense recap

`sync::run` assembles the three outcomes into a result block, replacing the
`collect_for_sync` + `print_summary` call on the export path:

```
synced: last-sync 00000000 → 02a021f7

imported from $HOME    2 files   (.bashrc modified, .profile added)
merge                  created merge commit (test-host into origin/main)
exported to $HOME      1 file    (.vimrc added)
```

Each row degrades to a plain-language idle form:

```
imported from $HOME    nothing — $HOME matched last-sync
merge                  none (target already contained the import)
merge                  fast-forward to target
exported to $HOME      nothing — $HOME already matched
```

The whole-no-op case collapses to one line:

```
synced: already up to date — $HOME, repo, and target agree (last-sync 02a021f7)
```

### 3. `status` leads with a state verdict

Add a derived state to the summary and lift the already-computed `resume_state`
(`src/status.rs:75`) onto it so the renderer does not recompute it:

```rust
pub(crate) enum SyncState {
    UpToDate,
    LocalChanges,    // $HOME drifted, target not ahead
    Incoming,        // target ahead, $HOME matches base
    Diverged,        // both
    MergePrepared,   // current-import present, @ is the merge, last-sync behind
    Conflict,        // conflicts at @
    Blocked,         // ResumeState::Blocked
    RepoDirty,       // repo_clean == Some(false)
}
```

Every arm is derivable from fields `SyncStatusSummary` already carries —
`home_differs_from_base`, `target_already_applied` / `target_differs_from_base`,
`current_import.revision`, `has_conflicts`, `repo_clean` — plus the lifted
`resume_state`. No new jj queries are introduced.

`print_summary` (`src/status.rs:212`) is reframed to: a `state:` headline, the
base/target/import/@ facts, the change lists, and a single `sync will:` line
driven by the state. The empty/missing base is rendered as `none` rather than
the raw all-zero id (`00000000`):

```
state:   incoming — target is 2 changes ahead of last-sync

base:    last-sync     02a021f7
target:  origin/main   7ce3d1b6
import:  none
repo:    /path/to/repo  (clean)

incoming changes (target since base):
  - modified .bashrc
  - added    .vimrc
local changes ($HOME since base):
  - modified .zshrc

sync will:  merge target into the imported $HOME state, then export
```

The `MergePrepared` (resume) state instead reads:

```
state:   merge prepared — not yet exported

base:    last-sync     02a021f7
import:  current-import a1b2c3d4   (prepared at @)
@:       9f8e7d6c   (merge of import + target)

sync will:  export the prepared merge to $HOME and advance last-sync
```

Change lists keep the existing 8-line cap with the `… N more` overflow
(`src/status.rs:393-402`).

### Fallback and decline semantics

- `--no-export` (`src/sync.rs:47`) does not write `$HOME` and does not advance
  `last-sync`, so it prints a prepared-but-not-exported line
  (`prepared merge X; not exported (--no-export)`), not the recap.
- A merge that produces conflicts still errors before export
  (`src/sync.rs:41-45`); the recap is only reached on a clean export.
- The `Blocked` state reuses the existing reason strings verbatim (the
  "is not a descendant of base … inspect it with `jj log`" text), so the
  enforced precondition and the reported one stay identical.
- Diff-aware export never *deletes*: deletion candidates remain report-only, as
  today. Skipping an identical file is silent; it is not reported as exported.

## Non-goals

- Not adding machine-readable output (`--json`); this is human-facing text only.
- Not changing the sync algorithm, the import placement decision
  (`decide_import_placement`, `src/import.rs:196`), or the resume preconditions —
  only what those phases *return* and how it is printed.
- Not implementing `$HOME` deletion; deletion candidates stay report-only.
- Not adding color or terminal styling.
- Not touching `dotmerge add` output.

## Verification

- After a sync that imports local drift and writes one file, assert stdout
  contains `synced:` with the `last-sync` transition and an
  `exported to $HOME    1 file` row, and does **not** contain `next:`.
- Assert a no-op sync prints the single `already up to date` line and writes no
  files (check `$HOME` mtimes are unchanged across the run, proving diff-aware
  export skipped them).
- Assert a sync against a sibling target prints `created merge commit`, and a
  sync that only advances to an ancestor/descendant prints `fast-forward to
  target` / `none`.
- For status, assert the `state:` headline matches the scenario across at least
  `up to date`, `incoming`, `local changes`, `merge prepared`, and `blocked`,
  and that `blocked` shows the same reason `sync` errors with.
- Update existing string assertions in `tests/dotmerge_cli.rs`
  (`import: current-import = missing` at line 65, `target: already applied` at
  line 112, the resume-state wording around lines 181-201) to the new output.

## Success criteria

- `sync` output names what changed (imported / merged / exported) and never
  prints a `next:` forecast on the export path.
- `status` output leads with exactly one `state:` verdict and one `sync will:`
  line derived from it.
- Export writes only files whose `$HOME` content differs; a no-op sync touches
  no file mtimes.
- `status` and `sync` derive their state from the same `resume_state` /
  summary fields, with no second source of truth.

## Suggested implementation shape

1. Diff-aware export first (`src/fs.rs`, `src/export.rs`): smallest, isolated,
   independently testable, and unblocks the export row. Commit.
2. `MergeOutcome` (`src/merge.rs`) and `ImportOutcome` (`src/import.rs`):
   mechanical return-type changes; update the two `sync::run` call sites. Commit.
3. `sync::run` recap rendering (`src/sync.rs`), replacing the post-sync
   `collect_for_sync`/`print_summary` on the export path. Commit.
4. `SyncState` + lifted `resume_state` on `SyncStatusSummary` (`src/model.rs`),
   then reframe `print_summary` (`src/status.rs`); update `tests/dotmerge_cli.rs`
   assertions. Commit.

Steps 1-3 (sync recap) and step 4 (status) are independent once
`FileStatusSummary` is shared, so either half can land first.
