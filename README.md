# dotmerge

A small, conservative [jj](https://github.com/jj-vcs/jj)-backed dotfile sync tool.

`dotmerge` maps a jj repo root directly onto your `$HOME` and reconciles the two
through real version control instead of file-by-file copying:

```text
repo/.zshrc               <->  ~/.zshrc
repo/.config/sway/config  <->  ~/.config/sway/config
repo/bin/foo              <->  ~/bin/foo
```

`$HOME` is **not** a jj/git worktree — files are copied, not symlinked. Each
`dotmerge sync` imports your local home changes, merges them with a target
revision, and exports the result back to `$HOME`. Because the merge happens in
jj, real conflicts are resolved with normal jj workflows rather than by
clobbering files in `$HOME`.

It is designed to be hard to accidentally corrupt `$HOME`: it only touches
managed paths, never deletes home files, and only advances its sync marker after
a successful export.

> For the full design and rationale, see [`docs/dotmerge.md`](docs/dotmerge.md).

## How it works

Every command reasons about three coordinates and two jj bookmarks:

```text
base   = last-sync, or the empty tree on the first sync
target = the revision you sync against (e.g. main@origin)
repo   = the jj repo root that mirrors $HOME
home   = the actual files under $HOME
```

- **`last-sync`** — a local bookmark for the last revision known to match this
  machine's `$HOME`. It only moves after a successful export.
- **`current-import`** — a local bookmark for the in-progress imported home
  snapshot. It lets `dotmerge` resume an interrupted or conflicted sync.

Neither bookmark is meant to be pushed or shared between machines.

A full sync is:

```text
import home changes onto base  ->  merge with target  ->  export back to $HOME  ->  advance last-sync
```

Only managed paths participate: paths already in the target revision, plus paths
you have explicitly admitted with `dotmerge add`. Unrelated files in `$HOME` are
ignored.

## Install

`dotmerge` is installed from source with a recent Rust toolchain. You also need
[`jj`](https://github.com/jj-vcs/jj) installed to drive the repo.

```bash
cargo install --git https://github.com/dstoc/dotmerge
```

## Configuration

A config file supplies optional defaults so you don't have to retype the
coordinates. It is TOML with three optional keys:

```toml
# ~/.config/dotmerge/config.toml — all keys optional
home   = "~/dotmerge-home"   # overrides $HOME
repo   = "~/dotmerge"
target = "main@origin"
```

- Path values (`home`, `repo`) must be absolute or begin with `~/` (expanded
  against the real `$HOME`). Unknown keys are a hard error.
- The config file is located via `--config FLAG` > `DOTMERGE_CONFIG` env >
  `$XDG_CONFIG_HOME/dotmerge/config.toml` (falling back to
  `~/.config/dotmerge/config.toml`).

Each coordinate is resolved independently as `--flag` > config value > fallback:

| Coordinate | Fallback                                              |
| ---------- | ----------------------------------------------------- |
| `home`     | the real `$HOME`                                      |
| `repo`     | none — required via flag or config                    |
| `target`   | none — required for `status`/`sync`; `add` uses it only as a guard |

## Commands

### `dotmerge status [--target REV] [--repo PATH] [--home PATH]`

Inspects the current sync state without changing anything. It leads with a
single headline state — one of `up to date`, `local changes`, `incoming`,
`diverged`, `merge prepared`, `conflict`, `blocked`, or `repo dirty` — followed
by the base/target/import/repo facts, the incoming and local change lists, and a
single `sync will:` line naming the next move.

### `dotmerge sync [--target REV] [--repo PATH] [--home PATH]`

Runs the full `import -> merge -> export` workflow and advances `last-sync`. It
requires a clean repo working copy and reports what actually changed: what was
imported from `$HOME`, whether a merge commit was created, and which files were
written back.

### `dotmerge sync --no-export ...`

Runs the repo-side phases (import + merge) and stops before writing to `$HOME`.
It prepares `current-import` and the merge, leaves the working copy at the
prepared result, and does **not** move `last-sync`. A later full `dotmerge sync`
finishes the job.

### `dotmerge add PATH... [--repo PATH] [--home PATH]`

Admits one or more new local files into sync by copying them into the repo at
their corresponding repo-relative paths. This is how a file that exists in
`$HOME` but isn't tracked yet becomes managed. `add` preserves executable bits
and symlink identity, and does not require a clean working copy. Paths may be
absolute, `~/`-prefixed, or relative to the current directory; either way the
resolved location must be inside `$HOME`.

Because `add` writes into the `@` working copy, it refuses to run when `@` is a
sync-critical revision — at or below `last-sync`, at or below the configured
target, or exactly `current-import` — since that would rewrite synced/target
history or pollute an in-progress import. Start a fresh change with `jj new`
first.

## Workflow
`dotmerge` output and recommended `jj` workflows are still being refined. Here are some recipes:

Start from scratch, set up a `dotmerge` config on the first machine:
```bash
mkdir -p ~/.config/dotmerge
cat > ~/.config/dotmerge/config.toml <<EOF
repo   = "~/dotmerge"
target = "main@origin"
EOF
```

Create a local repo for a newly created/emptty github repo:
```bash
jj git init --colocate ~/dotmerge
cd ~/dotmerge
jj git remote add origin git@github.com:<you>/dotfiles.git
```
Track the dotmerge config and push it as `main`:
```bash
dotmerge add ~/.config/dotmerge/config.toml
jj commit -m "Init dotfiles repo with dotmerge config"
jj git push --named main=@-
```

Now, to sync to a second machine:
```bash
jj git clone git@github.com:<you>/dotfiles.git ~/dotmerge
cd ~/dotmerge
jj new 'root()'
# explicit args since we don't have a config until after this step
dotmerge sync --repo ~/dotmerge --target main@origin
```

Then when you need to sync:

Pull remote changes:
```bash
jj git fetch
```

Sync with the local files:
```bash
dotmerge sync
```

Push local changes:
```bash
jj bookmark move main --to @
jj git push --tracked
```

## Resolving conflicts

A conflict means a managed path changed both in your local `$HOME` and in the
target since `last-sync`. When that happens, `dotmerge sync` stops without
touching `$HOME`:

```text
merge produced jj conflicts at `@`; resolve them in the repo, then rerun `dotmerge sync`
```

The conflicted merge is left at `@` and `current-import` is preserved, so you
resolve it in the repo with normal jj tools:

```bash
cd ~/dotmerge
jj status          # see the conflicted files at @
# edit the files to resolve, or use `jj resolve`
dotmerge sync      # rerun: refreshes the import, rebases the merge, then exports
```

On rerun, `dotmerge` refreshes the import from your current `$HOME` and rebases
the prepared merge onto it, **preserving your resolution**. If `$HOME` didn't
change the conflicting path, the merge stays clean and exports; if it did, the
conflict is re-raised for you to resolve again. `last-sync` only advances once
the export succeeds.

## Safety notes

- `last-sync` only moves after a fully successful export, so an interrupted sync
  never leaves the marker ahead of `$HOME`.
- `dotmerge` never deletes files in `$HOME` and never overwrites unresolved
  conflicts; home files are written atomically.
- Only managed paths are touched — unrelated home-directory churn is ignored.
- It assumes managed files in `$HOME` are not being edited concurrently during a
  run.
- Full deletion handling, host-specific variants, secrets, and templating are
  intentionally out of scope for now.
