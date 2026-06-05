## Proposal: `dotmerge`

`dotmerge` is a small jj-native dotfile sync tool.

The selected repo root maps directly onto `$HOME`:

```text
repo/.zshrc              <->  ~/.zshrc
repo/.config/sway/config <->  ~/.config/sway/config
repo/bin/foo             <->  ~/bin/foo
```

`$HOME` is **not** a jj/git worktree, and files are copied rather than symlinked.

The core idea is:

```text
base   = last-sync, or the empty tree on first sync
target = revision to merge with, passed explicitly to each command
repo   = repo root, passed explicitly to each command
home   = actual filesystem
```

`last-sync` is a local jj bookmark recording the last revision known to match this clone's paired home directory.

`current-import` is a local jj bookmark recording the current imported home state being merged.

It is not meant to be pushed or shared between machines.

---

## MVP goals

The MVP should be conservative, explicit, and hard to accidentally corrupt `$HOME`.

It should support:

```text
dotmerge status --target REV --repo PATH
dotmerge sync --target REV --repo PATH
dotmerge sync --no-export --target REV --repo PATH
dotmerge add PATH... --repo PATH
```

`--repo`, `--target`, and `--home` may be omitted when a config file supplies
them (see [Config](#config)); the flags override the config when both are
present.

No templating, no scripts, no host-specific variants, no automatic background commits, no fancy merge UI.

---

## State model

The sync state is recorded in local jj bookmarks:

```text
last-sync
current-import
```

Meaning:

```text
last-sync      = the last revision known to match this local home directory
current-import = the current imported home snapshot being merged
```

The target revision is not implicit.

It must be supplied to commands that need it (`status`, `sync`) — either via
`--target` or the `target` config key, for example:

```text
--target origin/main
```

`--target` must resolve to exactly one revision.

For `sync` and `sync --no-export`, a "clean repo working copy" means there are no unrelated repo edits outside the current dotmerge sync state.

Normally the three important states are:

```text
base   = jj revision last-sync
target = jj revision from --target or the config `target`
repo   = filesystem path from --repo or the config `repo`
home   = filesystem under $HOME (or --home / the config `home`)
```

On first sync, substitute the empty tree for `last-sync`.

On first use, `last-sync` may not exist yet.

In that case, `dotmerge` should treat the base as:

```text
the empty tree
```

So initial sync becomes:

```text
empty tree -> import current managed home state -> merge with target -> export
```

The critical invariant is:

```text
last-sync only moves after a successful export to home
```

Intermediate imported or merged revisions are not automatically the new last-sync revision.

`current-import` is allowed to move during sync retries as home state changes.

---

## Sync model

`dotmerge sync --target REV --repo PATH` should be thought of as:

```text
import home changes onto base
merge imported state with target
update the repo working copy to the sync state
export merged result back to home
advance last-sync to the exported revision
```

That is different from a simple file-by-file copier.

The repo history becomes the source of truth for reconciliation, while `$HOME` remains an external filesystem projection.

`current-import` is the mechanism that lets `dotmerge` resume a previously interrupted sync without guessing which repo revision represents the imported home state.

For MVP, `dotmerge` assumes the managed files in `$HOME` are not being modified concurrently during a run.

If they are, behavior is undefined.

---

## Managed paths

`dotmerge` should not use include/exclude config.

Instead, managed paths are derived from jj trees.

For normal sync, first normalize `current-import` so it is a direct child of the current clean repo-side state.

Then a path is managed if it appears in:

- the parent of `current-import` after that normalization
- the target revision

If the requested target revision is already an ancestor of that repo-side parent, managed paths may be derived from the repo-side parent alone.

This means:

- existing tracked dotfiles come from the jj history itself
- deletions remain visible because managed paths are still compared against `last-sync`
- clean repo-side additions already present in the repo-side parent are admitted into sync
- random files elsewhere in `$HOME` are ignored
- removing a path from the repo-side parent can mean "stop managing this path", not "delete it from `$HOME`"

Unmanaged files in `$HOME` should be ignored completely by `status`, `sync --no-export`, and `sync`.

New local files are not imported automatically just because they exist in `$HOME`.

To admit a new local file into sync, the user must first make it part of the clean repo working-copy state, for example with:

```text
dotmerge add PATH --repo ~/dotmerge-repo
```

That keeps ordinary sync conservative and avoids accidental imports from unrelated home-directory churn.

---

## Sync phases

### 1. Import

Import answers:

```text
what changed in home since last-sync?
```

Conceptually:

```text
base -> home
```

The import phase should:

- compare managed paths in `$HOME` against `last-sync`
- detect additions, modifications, and deletions
- materialize those home changes as a jj working-copy change or commit on top of `last-sync`
- move `current-import` to that imported revision when the imported tree differs from the current repo-side state

If there are no home changes, import is a no-op.

If `last-sync` does not exist yet, import should compare `$HOME` against the empty tree and materialize a home snapshot revision.

If `current-import` already exists from a previous interrupted sync, rerunning `dotmerge sync` should first normalize it so that `current-import` is a direct child of the current repo-side state, then refresh that imported revision from the current managed `$HOME` state.

If the refreshed import would be tree-identical to the current repo-side state, `dotmerge` should abandon `current-import` instead of keeping an empty or redundant import commit.

`current-import` represents imported home state, not a fixed target choice, so it may be reused even if the user reruns `dotmerge sync` with a different `--target`.

After resolving conflicts in jj, the user may rerun `dotmerge sync` directly as long as the repo still satisfies the sync resumability checks.

On initial sync, the imported home snapshot should still be limited to managed paths. It should not blindly import all of `$HOME`.

In practice, that means:

- paths already present in the target revision are in scope automatically
- additional local-only paths come into scope once they are part of the current clean `@` revision, commonly via `dotmerge add PATH`
- if a managed path from `target` or `@` is missing from `$HOME`, treat that as a home-side deletion candidate
- if `target <= @`, a path removed from `@` drops out of managed scope and should be left alone in `$HOME`

More precisely, import should treat the current repo-side state as the parent of `current-import` after normalization. Managed paths come from:

- the parent of `current-import`
- plus the target revision when the target is not already an ancestor of that repo-side parent

Important constraint:

```text
the imported revision is not yet last-sync
```

It only represents:

```text
base plus local home edits
```

On first sync, that means:

```text
empty tree plus current managed home contents
```

Open question for the MVP:

```text
should dotmerge require a clean working copy before import?
```

For MVP, yes.

`dotmerge sync` should require a clean jj working copy in `--repo` before it begins.

That means:

- no unrelated local repo edits are present
- any clean repo state already present at `@` is eligible to become part of the current dotmerge sync state
- `dotmerge` does not have to guess how to combine sync state with user-authored repo changes
- interrupted sync state is tracked via `current-import`, not via arbitrary dirty working-copy content

For MVP, `dotmerge` should prefer erroring over trying to be clever.

### 2. Merge

Merge answers:

```text
how do local home edits combine with the target revision?
```

Conceptually:

```text
merge(base+home-edits, target)
```

This phase should:

- take the imported revision
- merge it with the revision passed via `--target`
- reuse the imported revision directly when the requested target is already an ancestor of it
- ask jj whether the merge result contains conflicts
- stop if jj reports conflicts
- let the user inspect and resolve conflicts using normal jj workflows

The user may then create or finalize a merge commit.

This means conflict resolution happens in jj first, not while writing into `$HOME`.

That is a good property: it keeps merge logic in the VCS layer and keeps home export conservative.

If the imported or merged change is redundant because an existing revision already represents the desired result, `dotmerge` should abandon the temporary change and reuse the existing revision instead.

In particular, if the requested target revision is already an ancestor of the prepared imported state, no separate merge commit is needed.

The merge should conceptually be:

```text
merge(current-import, target)
```

### 3. Export

Export answers:

```text
how do we make home match the merged revision?
```

Conceptually:

```text
merged revision -> home
```

The export phase should:

- copy managed files from the merged revision into `$HOME`
- skip files whose `$HOME` content already matches the merged revision, so an unchanged path is left untouched and a no-op sync writes nothing
- write home files atomically
- create parent directories as needed
- preserve executable bits
- never overwrite unresolved conflicts

If export succeeds completely, then:

```text
last-sync -> exported merged revision
```

If export fails, `last-sync` must not move.

After a successful export, `current-import` should be cleared.

Export should be atomic per file, but not necessarily across the whole sync.

That means an export failure may leave some home files already updated while others are not.

---

## Command behavior

### `dotmerge status --target REV --repo PATH`

Shows where the three sync phases stand without changing anything.

It should inspect the current `$HOME` state directly each time it runs.

It should hard error if `--target` does not resolve to exactly one revision.

Status should report:

- whether `last-sync` exists
- whether `current-import` exists
- if it does not exist, that initial sync will use the empty tree as base
- whether `$HOME` differs from `last-sync`
- whether the target revision differs from `last-sync`
- whether an existing `current-import` is cleanly resumable
- whether the current sync state contains jj conflicts
- whether import and/or merge state already exists in the repo
- whether deletion candidates are present
- whether a sync would require import, merge, export, or conflict resolution

Status leads with a single headline state — one of `up to date`, `local changes`, `incoming`, `diverged`, `merge prepared`, `conflict`, `blocked`, or `repo dirty` — followed by the base/target/import/repo facts, the incoming and local change lists, and a single `sync will:` line naming the one next move (replacing an enumerated multi-step plan).

Example high-level output:

```text
state:   incoming — target is 1 change ahead of last-sync

base:    last-sync     qpvuntsm
target:  origin/main   mzytrlsq
import:  none
repo:    ~/dotmerge-repo  (clean)

incoming changes (target since base):
  - modified .config/sway/config
local changes ($HOME since base):
  - modified .zshrc
  - added    .config/kitty/kitty.conf

sync will:  merge target into the imported $HOME state, then export
```

A missing `last-sync` is shown as `none` rather than the empty-tree id. The `merge prepared` state additionally shows the prepared `@` revision. The `blocked` state shows the same reason `sync` would error with.

If conflicts are expected or already present, status should say so explicitly.

It does not need to enumerate every managed path in the MVP. A concise summary like the example above is sufficient.

If deletion candidates are present, status should mention that explicitly.

If `last-sync` does not exist yet, status should say that this is an initial sync from the empty tree.

If `current-import` exists, status should report that sync will normalize it to a direct child of the current repo-side `@` revision before refreshing it from `$HOME`.

Here, "repo working copy is clean" means there are no filesystem modifications relative to `@`.

### `dotmerge sync --no-export --target REV --repo PATH`

Runs the repo-side parts of sync but stops before writing to `$HOME`.

It should require the same clean-repo preconditions as `dotmerge sync`.

It should hard error if `--target` does not resolve to exactly one revision.

It should:

- create or refresh `current-import` when import produces a distinct imported state
- perform the merge against the requested target
- leave any resulting conflict state in the repo for the user to resolve
- leave the prepared sync state in place for a later full `dotmerge sync`
- update the repo working copy to the prepared merge result
- not export to `$HOME`
- not move `last-sync`

A later full `dotmerge sync` should still refresh `current-import` again from the current managed `$HOME` state before exporting.

After it completes, it should show the status summary for the prepared state, which reports the `merge prepared` state.

### `dotmerge sync --target REV --repo PATH`

Performs the full workflow:

```text
import -> merge -> export
```

Suggested behavior:

1. Verify preconditions.
   - the repo at `--repo` must have a clean jj working copy
   - if `current-import` exists, it must satisfy the resume preconditions reported by `status`
   - otherwise, error, report which check failed, and require manual repair; do not silently discard or rewrite `current-import`
2. Resolve the sync base:
   - `last-sync`, if it exists
   - otherwise the empty tree
3. Normalize `current-import` so it is a direct child of the current clean repo-side `@` revision.
   - if the existing bookmark is already there, it may be rewritten in place
   - otherwise, replace it with a fresh import commit at that position
   - if the current repo-side `@` is just a disposable empty placeholder (single parent, empty description, empty tree), sync may rewrite through it rather than preserving it in history
4. Compute managed paths from the repo-side parent state.
   - if the requested target is already an ancestor of that repo-side parent, use only the parent tree's paths
   - otherwise, use the union of that parent tree's paths and the target tree's paths
5. Refresh `current-import` from the current managed home state on top of `last-sync`.
   - `current-import` should remain an imported home snapshot, not absorb repo-side commits that were already above an older import
   - if that refreshed import would be tree-identical to the repo-side parent state, abandon `current-import` and reuse the repo-side state directly
6. Merge the prepared import state with the target revision.
   - if the target revision is already an ancestor of the prepared state, reuse that prepared state directly instead of creating a merge commit
7. If merge conflicts exist, stop and report them. Persist the conflicted merge at `@` and leave `current-import` in place (it remains a parent of that merge) so the prepared state survives for the user to resolve. `last-sync` does not move.
8. If the user later resolves those conflicts in jj and reruns `dotmerge sync`, the prepared merge at `@` — a merge whose parent is `current-import` — is itself a resumable shape. `dotmerge` refreshes `current-import` in place from the latest managed home state (keeping its base parent) and rebases the prepared merge onto it, rather than building a fresh merge. Because jj re-applies the merge's recorded resolution, an unchanged `$HOME` keeps the merge clean and export proceeds, while a `$HOME` that changed the conflicting path re-raises the conflict to be resolved again.
9. If the merge result is clean, export it to `$HOME`.
10. Only after successful export, move `last-sync` to the exported revision.
11. Clear `current-import` if it still exists.

After a successful sync, leave the repo working copy at the final merged/exported revision.

After a successful export, it should report what changed rather than a forecast: what was imported from `$HOME`, whether a merge commit was created (or a fast-forward, or nothing), and which files were written back — only the files that actually differed from `$HOME`. It also shows the `last-sync` transition, and collapses to a single "already up to date" line when nothing moved.

If the imported change or merge result is redundant because an existing revision already represents the desired result, `dotmerge` should reuse that existing revision instead of creating a new merge commit.

`dotmerge sync` should not silently auto-commit unrelated repo state.

`dotmerge sync` is allowed to update the repo working copy as part of creating, refreshing, and merging sync state.

If sync stops on conflicts, it should leave the repo working copy at the conflicted merge state for the user to resolve there.

If the import or merge step requires the user to confirm or finalize a commit, that should be explicit.

For MVP, it is acceptable if conflict resolution is manual and requires rerunning `dotmerge sync` after jj conflicts are resolved.

### `dotmerge add PATH... --repo PATH`

Admits one or more new local files into sync.

This is the escape hatch for files that exist in `$HOME` but are not yet tracked by the target tree.

`dotmerge add` is separate from `dotmerge sync`.

It is a repo-editing workflow, not a sync workflow.

It does not require a clean repo working copy.

Conceptually, it is just a helper for copying one or more home files into the corresponding repo-relative paths so they become part of the next clean `@` state.

Suggested behavior:

1. Verify that `@` is a fresh change, not a sync-critical revision. `add` writes
   into the `@` working copy, so it must refuse when `@` is at or below
   `last-sync`, at or below the configured target, or exactly `current-import` —
   otherwise the copy would rewrite synced/target history or pollute the
   in-progress import. The fix is to start a fresh change (`jj new`) first. A
   target that is not configured, or that does not resolve yet (e.g. during
   bootstrap), is simply not checked.
2. Verify that each `PATH` is inside `$HOME`.
3. Verify that each `PATH` is a file and currently exists in `$HOME`.
4. Verify that each corresponding repo-relative path does not already exist in the repo.
5. Copy or stage each file into the repo working copy selected by `--repo` at the corresponding repo-relative path.
6. Make those files visible to the next import/merge/export cycle.

When copying into the repo, `dotmerge add` should preserve executable bits and symlink identity. A managed symlink may point outside `$HOME`: `add` only requires the symlink's own location to be inside `$HOME`, and records the link verbatim rather than copying whatever it points to.

`PATH` may be absolute, `~/`-prefixed, or relative to the current working directory. A relative `PATH` is resolved against the directory `dotmerge` is invoked from, and the resolved location must land inside `$HOME` or `add` rejects it.

`dotmerge add` should be explicit and narrow. It is how new local files enter the managed set.

If the path already exists in the repo, `dotmerge add` should error and direct the user to use `dotmerge sync` instead.

If any requested path is invalid, `dotmerge add` should fail the whole command without partially adding files.

A normal flow could be:

1. `dotmerge add` one or more new files
2. inspect the repo changes
3. commit them in jj
4. run `dotmerge sync --target ... --repo ...`

## Config

A config file supplies optional defaults for the three coordinates, so a
machine that always syncs one repo against one target need not retype them. No
config file is required; running with the flags alone still works.

### File

TOML, with three optional keys:

```toml
# ~/.config/dotmerge/config.toml — all keys optional
home   = "~/dotmerge-home"   # overrides $HOME
repo   = "~/dotmerge-repo"
target = "origin/main"
```

Unknown keys are a hard error (a typo such as `tagret =` fails loudly rather
than being silently ignored).

Path values (`home`, `repo`) must be absolute or begin with `~/`. A leading
`~/` expands against the **real** process `$HOME` — always the real one, never
the `home` override, so `home = "~/dotmerge-home"` is well-defined and tilde
expansion means the same thing everywhere. Any other relative path is an error.

### Resolving values

Each of `home`, `repo`, `target` is resolved independently:

```text
--flag  >  config value  >  fallback
```

- `home` falls back to the real `$HOME`.
- `repo` has no fallback: missing everywhere is an error.
- `target` has no fallback for `status`/`sync`: missing everywhere is an error.
  `add` does not require `target`, but uses a configured target (when present)
  as a safety guard — it refuses to add onto the target or its ancestors.

### Locating the config file

The config *location* is resolved separately from its *values*:

```text
--config FLAG  >  DOTMERGE_CONFIG env  >  default path
```

The default path is `$XDG_CONFIG_HOME/dotmerge/config.toml`, falling back to
`$HOME/.config/dotmerge/config.toml` when `$XDG_CONFIG_HOME` is unset or empty.
The default path is always derived from the real process `$HOME`/
`$XDG_CONFIG_HOME`, never from the `home` override (which would be circular).

A `--config` or `DOTMERGE_CONFIG` path that does not exist is an error — the
caller named a file that is not there. A missing *default* path is not an
error; it is treated as an empty config, so the no-config case stays
frictionless.

---

## jj integration

Use `jj-lib` for the MVP.

This design now depends on first-class access to:

- revisions and revision expressions
- trees and tree equality
- bookmark read/write operations
- creating or refreshing imported changes
- merge results and conflict state

That is a better fit for the jj library layer than for parsing CLI output from subprocesses.

The tool should not depend on Git refs directly.

The underlying repo may be Git-backed, but `dotmerge` should speak jj through `jj-lib`.

The jj layer lives in `jj.rs` and is the only module that imports `jj-lib`; the
rest of `dotmerge` speaks plain data types. It is split into:

- `JjClient`, a thin handle that locates the workspace (`open`, `begin`,
  `repo_path`, `working_copy_path`).
- `JjSession`, returned by `begin()` after loading the workspace once. It owns a
  single jj `Transaction`; every read and mutation in a command goes through it,
  and `finish()` commits exactly one jj operation — so one `dotmerge sync` is one
  `op log` entry.

Revisions are carried internally as a `CommitId`; identity is commit-id
equality, not revset-string comparison. Because export to `$HOME` runs before
`finish()`, a failed export leaves the transaction uncommitted and `last-sync`
unmoved, which is what enforces the "last-sync only moves after a successful
export" invariant.

If `--repo` does not point to a valid jj repo, `dotmerge` should hard error.

If `--target` does not resolve to exactly one revision, `dotmerge` should hard error.

---

## Conflict handling in MVP

MVP should ask jj for conflict state and hand conflict resolution off to jj.

A conflict means some managed path changed both:

- in local home edits since `last-sync`, and
- in the target revision since `last-sync`

If `last-sync` does not exist yet, conflicts mean:

- the imported home snapshot changed a path from the empty-tree base, and
- the target revision also changed that path from the empty-tree base

For a conflict, report something like:

```text
conflict .config/git/config

local home changed since last-sync
target changed since last-sync

resolve in jj, then rerun dotmerge sync
```

On initial sync, this should still be treated as a normal jj merge conflict. Only the wording should differ, referring to the empty-tree base rather than to `last-sync`.

When rerun after conflict resolution, `dotmerge sync` should:

- locate `current-import` and the prepared merge that sits on top of it at `@`
- refresh `current-import` in place from the current managed `$HOME` state, keeping its base parent
- rebase the prepared merge onto the refreshed import rather than building a fresh merge, so the recorded resolution is preserved
- stop again if the rebase re-raises a conflict (which happens when `$HOME` changed the conflicting path since the resolution)
- otherwise continue to export

The user is expected to resolve conflicts in jj and then rerun `dotmerge sync`.

For MVP, it is acceptable to leave all conflict resolution manual.

Later, add:

```text
dotmerge sync --target-wins
dotmerge sync --home-wins
dotmerge merge
dotmerge reset-import
```

But not needed initially.

---

## File handling rules

MVP should support:

```text
regular files
directories
symlinks
executable bit
missing files
```

For safety:

```text
write home files atomically
create parent directories as needed
never overwrite unresolved conflicts
never delete home files unless explicitly supported later
preserve executable bit from exported revision -> home
```

Modification times are not part of sync semantics.

Deletion can wait.

Deletion semantics are still tricky:

```text
home deleted since base
target deleted since base
both deleted
deleted on one side, modified on the other
```

For MVP, report deletion candidates but require a later explicit design.

---

## Implementation language

Build the MVP in **Rust**.

Shell is fine for a quick spike, but this tool writes into `$HOME`, tracks path mappings, stages imported filesystem content, and needs careful error handling. Rust will make the real version easier to test and safer to evolve.

The crate layout:

```text
src/main.rs     command dispatch
src/cli.rs      argument parsing
src/model.rs    data types
src/util.rs     home / hostname helpers
src/jj.rs       jj-lib plumbing (JjClient + JjSession)
src/import.rs   import phase + placement
src/merge.rs    merge phase
src/export.rs   export phase
src/status.rs   status output + clean-copy check
src/fs.rs       filesystem read/write + path safety
src/add.rs      `dotmerge add`
```

There is no `config.rs` yet (config is deferred).

---

## Deferred features

Leave these out initially:

```text
automatic commits
interactive merge UI
templating
host-specific files
secret management
script hooks
push/pull integration
per-path last-sync state
syncing last-sync markers across machines
full deletion support
binary merge handling
watch mode
```

Those can come later if the core model feels good.

---

## MVP summary

`dotmerge` should start as:

```text
a conservative jj-backed import/merge/export tool for dotfiles
```
