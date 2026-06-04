# Proposal: config file for home / repo / target defaults

## Motivation

Every `dotmerge` invocation that touches a repo must pass `--repo`, and
`status`/`sync` must also pass `--target` (`src/cli.rs:25-34`,
`src/cli.rs:42-50`). `home` is not overridable at all — it is always `$HOME`,
read once via `util::home_dir()` (`src/util.rs:12`) at the top of each handler
(`src/status.rs:34`, `src/sync.rs:12`, `src/add.rs:16`).

For a tool whose whole job is "sync *this* repo onto *this* home against *this*
target," the three coordinates are stable across runs but must be retyped every
time:

```
dotmerge status --target origin/main --repo ~/dotmerge-repo
dotmerge sync   --target origin/main --repo ~/dotmerge-repo
```

The design doc already anticipates this — `## Config` (`docs/dotmerge.md:509`)
says "the MVP should not require a config file … later, a config file can
provide defaults for these." This proposal is that file.

## Problem statement

Three coupled gaps:

1. **No persistent defaults.** `--repo` (always) and `--target` (for
   `status`/`sync`) are required on every call, even though a given machine
   syncs one repo against one target almost always.

2. **`home` cannot be overridden.** `util::home_dir()` reads `$HOME`
   unconditionally. There is no way to point `dotmerge` at a home directory
   other than the process `$HOME` — useful for testing, for a non-login service
   account, or for managing a second home tree.

3. **No config-discovery mechanism.** Even once a config exists, there is no
   way to choose *which* config (a `--config` flag, an env var) or to know where
   the default lives.

## Proposal

A TOML config file supplies optional defaults for `home`, `repo`, and `target`.
A `--home` flag is added to match the existing `--repo`/`--target`. Each value
is resolved independently; the config *location* is resolved separately from the
config *values*.

### File location and format

Default path: `$XDG_CONFIG_HOME/dotmerge/config.toml`, falling back to
`$HOME/.config/dotmerge/config.toml` when `$XDG_CONFIG_HOME` is unset or empty.
The default path is always derived from the **real process `$HOME`** (or
`$XDG_CONFIG_HOME`) — never from the `home` value inside the config, which would
be circular.

```toml
# ~/.config/dotmerge/config.toml — all keys optional
home   = "~/dotmerge-home"     # overrides $HOME
repo   = "~/dotmerge-repo"
target = "origin/main"
```

Deserialized with `serde` + `toml` into:

```rust
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub home: Option<String>,
    pub repo: Option<String>,
    pub target: Option<String>,
}
```

`deny_unknown_fields` makes typos (`tagret = "…"`) a hard error rather than a
silently ignored key.

### Path values: absolute or `~/`, never relative

`home` and `repo` accept either an absolute path or a path beginning with `~/`.
A leading `~/` expands against the **real process `$HOME`** — always real, never
the `home` override, so `home = "~/dotmerge-home"` is well-defined and tilde
expansion is identical everywhere. Any other relative path (`./repo`, `../x`,
`foo`) is an error:

```
config `repo`: paths must be absolute or start with `~/`, got `../repo`
```

The same expansion applies to the `--home`/`--repo` flag values (so a quoted
`--repo '~/x'` that the shell did not expand still works); a bare `~` shell-
expands before `dotmerge` sees it, so only the explicit-`~/` case needs handling.

### Two precedence ladders

**Config location** (where the file is):

```
--config FLAG  >  DOTMERGE_CONFIG env  >  default path
```

- `--config` or `DOTMERGE_CONFIG` pointing at a **missing** file is an error —
  the caller named a file that is not there.
- The **default** path missing is not an error: it yields an empty
  `FileConfig::default()` (the no-config case stays frictionless).

**Each value** (`home`, `repo`, `target`):

```
--flag  >  config value  >  fallback
```

- `home` fallback: the real `$HOME` (current behavior).
- `repo` fallback: none — error `--repo is required (no repo in config)`.
- `target` fallback: none (for `status`/`sync`) — error
  `--target is required (no target in config)`. `add` does not use `target`.

### CLI surface

`--config` is a global argument on `Cli` (`src/cli.rs:10`), since it is
cross-cutting and must be read before any subcommand resolves its values:

```rust
#[arg(long, global = true)]
pub config: Option<PathBuf>,
```

`--home` is added alongside `--repo`/`--target`. Because all three subcommands
now need `repo` and `home`, and `status`/`sync` additionally need `target`, the
flag fields become `Option` and resolution fills them in:

```rust
// RepoTargetArgs and AddArgs
#[arg(long)] pub home:   Option<PathBuf>,  // new
#[arg(long)] pub repo:   Option<PathBuf>,  // was required PathBuf
#[arg(long)] pub target: Option<String>,   // was required String (status/sync only)
```

### Resolution

A new `config` module owns discovery, parsing, `~/` expansion, and merge:

```rust
pub struct ResolvedConfig {
    pub home:   PathBuf,        // canonicalized, like util::home_dir today
    pub repo:   PathBuf,
    pub target: Option<String>, // None only for `add`
}

pub fn resolve(
    config_flag: Option<&Path>,   // --config
    home_flag:   Option<&Path>,
    repo_flag:   Option<&Path>,
    target_flag: Option<&str>,    // None for add
    need_target: bool,            // false for add
) -> Result<ResolvedConfig>;
```

`resolve` locates and loads the file (per the location ladder), then for each
value applies the value ladder, expands `~/`, and canonicalizes `home` and
`repo` with the same absolute-path + `canonicalize` checks `util::home_dir`
performs today (`src/util.rs:15-23`). The three handlers
(`status::run`/`sync::run`/`add::run`) call `resolve` once at the top in place
of `util::home_dir()` + reading `args.common.repo` / `args.common.target`.

`util::home_dir()` stays as the "real `$HOME`" primitive — `resolve` uses it for
the location default, for `~/` expansion, and as the `home` fallback.

## Non-goals

- **No per-value env vars** (`DOTMERGE_REPO`, etc.) — only `DOTMERGE_CONFIG` for
  the file location. The config file is the intended persistent mechanism.
- **No resolved-values / `dotmerge config` view.** The common case is "config
  alone"; flags and env are for debugging and power users, who can read the file
  directly.
- **No relative paths**, no `$VAR` interpolation beyond a leading `~/`.
- Not changing the sync/status/add algorithms — only how `home`/`repo`/`target`
  are obtained.
- No config-driven include/exclude lists (`docs/dotmerge.md:140` still holds).
- No multi-profile / multi-target config; one home, one repo, one target.

## Verification

- With a config providing all three, `dotmerge status` (no flags) resolves and
  runs against the configured repo/target; assert it behaves identically to
  passing the same values as flags.
- `--repo`/`--target`/`--home` flags override the corresponding config values
  (assert a flag value wins over a differing config value).
- A config with only `repo` still requires `--target` for `status`/`sync`, and
  errors with the `--target is required` message when absent; `add` works with
  just the configured `repo`.
- `--config` (and `DOTMERGE_CONFIG`) pointing at a nonexistent file errors;
  absence of the *default* file does not (a no-config run with `--repo`/`--target`
  flags still works).
- An unknown key in the TOML errors via `deny_unknown_fields`; a relative path
  in `repo`/`home` errors with the "absolute or start with `~/`" message.
- `home = "~/x"` expands against the real `$HOME` even when `--home`/config also
  set a different home (tilde never consults the override).
- `--config` precedence over `DOTMERGE_CONFIG` over the default is exercised by
  setting both and asserting the flag's file wins.

## Success criteria

- A populated `config.toml` lets `dotmerge status`/`sync`/`add` run with no
  repo/target/home flags.
- Flag > config > fallback holds for each of `home`/`repo`/`target`, and
  `--config` > env > default holds for the file location.
- Explicit-but-missing `--config`/env errors; missing default is silent.
- Unknown TOML keys and non-`~/` relative paths are hard errors.
- `~/` always expands against the real `$HOME`; config discovery never depends
  on the `home` override.

## Suggested implementation shape

1. Add `serde` (`features = ["derive"]`) and `toml` deps; add the `config`
   module with `FileConfig`, `~/` expansion, and the location ladder (loading +
   the missing-file rules), unit-tested in isolation. Commit.
2. Add `--config` (global), `--home`, and make `repo`/`target` `Option` in
   `src/cli.rs`; implement `config::resolve` (value ladder + canonicalize),
   reusing `util::home_dir`. Commit.
3. Switch `status::run`/`sync::run`/`add::run` to `config::resolve`, dropping
   their direct `util::home_dir()` + `args.common.*` reads; add CLI tests for
   the precedence, missing-file, unknown-key, and relative-path cases. Commit.
4. Replace the placeholder `## Config` section in `docs/dotmerge.md:509-518`
   with the resolved behavior. Commit.

Step 1 is self-contained and independently testable; steps 2-3 are the wiring;
step 4 is docs.
