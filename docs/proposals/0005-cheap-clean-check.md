# Proposal: cheaper working-copy cleanliness check

## Motivation

`dotmerge sync` refuses to start unless the repo working copy is clean
(`src/sync.rs:15`), and `dotmerge status` reports cleanliness
(`src/status.rs:45`). Both go through `JjClient::is_working_copy_clean`
(`src/jj.rs:394`).

## Problem statement

The current check is hand-rolled and reads everything into memory:

```rust
let tracked_paths = self.list_files(&current)?;          // every tracked path
let repo_paths = fs::list_repo_paths(self.repo_path())?; // full fs walk of repo
... // union them
for path in managed_paths {
    let fs_entry = fs::read_rooted_entry(self.repo_path(), &path)?; // reads full bytes
    let tree_entry = current_entries.get(&path)...;
    if tree_entry != fs_entry { return Ok(false); }
}
```

`read_rooted_entry` (`src/fs.rs:173`) reads each file's entire contents via
`fs::read`, and `read_entries_at_rev` materializes each tree entry's full
contents too (`src/jj.rs:691`). So the check is O(number of files × full file
size) on every `status` and every `sync`, even when the answer is "clean and
nothing changed." For a real dotfile repo with large files this is wasteful, and
it duplicates machinery jj already has.

It can also miss the first differing byte cheaply: equality compares whole
`ManagedEntry::File { contents }` values, so a 10 MB file that differs in byte 0
is still fully read on both sides before the comparison fails.

## Proposal

Prefer jj's own working-copy snapshot/diff to answer "is `@` clean", and fall
back to a metadata-gated comparison only where a direct content read is
unavoidable.

### Primary path: use jj's snapshot

`jj-lib` already tracks the working copy. Take a working-copy snapshot and ask
whether it differs from `@`'s tree, rather than re-deriving the diff by hand.
This reuses jj's ignore handling, size thresholds, and file-state cache, and
keeps dotmerge's notion of "clean" identical to `jj status`.

If snapshotting inside dotmerge is undesirable (it can mutate the working-copy
state file), expose a read-only comparison that walks jj's existing file states
instead of re-reading every byte from disk.

### Fallback path: gate content reads on metadata

Where a direct comparison is still needed (e.g. paths jj does not track but that
`list_repo_paths` surfaces), compare in cheapening order before reading bytes:

```text
1. entry kind (file / symlink / dir) differs        -> dirty
2. for files: size differs                          -> dirty
3. for files: executable bit differs                -> dirty
4. only then read contents and compare               -> maybe dirty
```

Short-circuit on the first mismatch so a clean repo never reads file bodies, and
a dirty repo stops at the first differing path instead of building the full
union and reading everything.

### Shared helper

`is_working_copy_clean` is the one consumer that needs a boolean. Keep its
signature, change only the internals. `status` already calls it once
(`src/status.rs:45`); `sync` calls it once (`src/sync.rs:15`). No callers need
the per-path detail today, so the cheap boolean is sufficient.

## Non-goals

- Not changing the *definition* of clean ("no filesystem modifications relative
  to `@`", per the doc); only how it is computed.
- Not adding a `--no-verify`/force flag.
- Not snapshotting the working copy as a side effect of `status` if that would
  surprise users — if jj's snapshot mutates state, prefer the read-only walk.

## Verification

- `dotmerge status` and `dotmerge sync` report the same clean/dirty verdict as
  before on every `tests/dotmerge_cli.rs` fixture.
- Add a test with a large unchanged file and assert the clean check does not
  read its full contents (e.g. via a wrapper that counts bytes read, or by
  asserting runtime stays flat as file size grows).
- A single-byte change at offset 0 of a large tracked file is still detected.

## Success criteria

- A clean repo answers `is_working_copy_clean` without reading any file body.
- The check short-circuits at the first differing path.
- dotmerge's "clean" matches `jj status` on the same repo.
