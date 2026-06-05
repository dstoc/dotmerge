use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

/// The subset of configuration that can be supplied via the config file.
/// All fields are optional; missing keys are silently treated as absent.
/// Unknown keys are a hard error (via `deny_unknown_fields`).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileConfig {
    pub(crate) home: Option<String>,
    pub(crate) repo: Option<String>,
    pub(crate) target: Option<String>,
}

/// Return the default config-file path.
///
/// Uses `$XDG_CONFIG_HOME/dotmerge/config.toml` when `$XDG_CONFIG_HOME` is
/// set and non-empty, otherwise `$HOME/.config/dotmerge/config.toml`.
/// The path is always derived from the real process `$HOME` (or
/// `$XDG_CONFIG_HOME`) — never from the `home` value inside any config,
/// which would be circular.
fn default_config_path() -> Result<PathBuf> {
    let base = if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        PathBuf::from(xdg)
    } else {
        // Fall back to the real $HOME.
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("$HOME is not set"))?;
        PathBuf::from(home).join(".config")
    };

    Ok(base.join("dotmerge").join("config.toml"))
}

/// Locate and load the config file, honouring the three-level ladder:
///
/// ```text
/// --config FLAG  >  DOTMERGE_CONFIG env  >  default path
/// ```
///
/// * If the path comes from the flag or the env var and the file is missing,
///   an error is returned (the caller explicitly named a file).
/// * If the default path is used and the file is missing, an empty
///   `FileConfig::default()` is returned (no-config runs stay frictionless).
pub(crate) fn load(config_flag: Option<&Path>) -> Result<FileConfig> {
    enum Source {
        Flag,
        Env,
        Default,
    }

    let (path, source) = if let Some(p) = config_flag {
        (p.to_path_buf(), Source::Flag)
    } else if let Some(val) = std::env::var_os("DOTMERGE_CONFIG").filter(|v| !v.is_empty()) {
        (PathBuf::from(val), Source::Env)
    } else {
        (default_config_path()?, Source::Default)
    };

    if !path.exists() {
        return match source {
            Source::Default => Ok(FileConfig::default()),
            Source::Flag => Err(anyhow!(
                "--config path does not exist: `{}`",
                path.display()
            )),
            Source::Env => Err(anyhow!(
                "DOTMERGE_CONFIG path does not exist: `{}`",
                path.display()
            )),
        };
    }

    // A file that exists at any of the three locations is parsed strictly:
    // parse errors (including unknown keys via `deny_unknown_fields`) always
    // surface. The default path is dotmerge-specific, so a malformed file there
    // is a real user error worth reporting, not something to silently ignore.
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config file `{}`", path.display()))?;

    toml::from_str(&text)
        .with_context(|| format!("failed to parse config file `{}`", path.display()))
}

/// Expand a leading `~/` in `raw` against the real process `$HOME`.
///
/// * An absolute path is returned unchanged.
/// * A path starting with `~/` has the `~/` replaced with `$HOME/`.
/// * Any other relative path (including bare `~`) is an error.
pub(crate) fn expand_path(raw: &str) -> Result<PathBuf> {
    if raw.starts_with('/') {
        return Ok(PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("$HOME is not set"))?;
        return Ok(PathBuf::from(home).join(rest));
    }
    Err(anyhow!(
        "paths must be absolute or start with `~/`, got `{raw}`"
    ))
}

/// The fully-resolved runtime coordinates for a single `dotmerge` invocation.
#[derive(Debug)]
pub(crate) struct ResolvedConfig {
    pub(crate) home: PathBuf,
    pub(crate) repo: PathBuf,
    /// The sync target, from the flag or config. `None` only when no target was
    /// supplied and the command did not require one (`add`, which still uses a
    /// configured target — when present — as a safety guard).
    pub(crate) target: Option<String>,
}

/// Resolve the three runtime coordinates (`home`, `repo`, `target`) for a
/// `dotmerge` invocation by applying the value ladder:
///
/// ```text
/// --flag  >  config value  >  fallback
/// ```
///
/// * `home` fallback: the real `$HOME` via [`crate::util::home_dir`].
/// * `repo` fallback: none — errors with `--repo is required (no repo in config)`.
/// * `target` fallback: none when `need_target` is `true` — errors with
///   `--target is required (no target in config)`.  When `need_target` is
///   `false` (the `add` subcommand), a missing target is not an error, but a
///   flag/config target is still returned (used only as a safety guard).
pub(crate) fn resolve(
    config_flag: Option<&Path>,
    home_flag: Option<&Path>,
    repo_flag: Option<&Path>,
    target_flag: Option<&str>,
    need_target: bool,
) -> Result<ResolvedConfig> {
    let file = load(config_flag)?;

    // --- home ---
    let home = if let Some(flag) = home_flag {
        let raw = flag
            .to_str()
            .ok_or_else(|| anyhow!("--home path is not valid UTF-8"))?;
        canonicalize_config_path(expand_path(raw)?, "--home")?
    } else if let Some(raw) = file.home {
        canonicalize_config_path(expand_path(&raw)?, "config `home`")?
    } else {
        crate::util::home_dir()?
    };

    // --- repo ---
    let repo = if let Some(flag) = repo_flag {
        let raw = flag
            .to_str()
            .ok_or_else(|| anyhow!("--repo path is not valid UTF-8"))?;
        canonicalize_config_path(expand_path(raw)?, "--repo")?
    } else if let Some(raw) = file.repo {
        canonicalize_config_path(expand_path(&raw)?, "config `repo`")?
    } else {
        return Err(anyhow!("--repo is required (no repo in config)"));
    };

    // --- target ---
    // Populated from the flag or config when available. `need_target` only
    // controls whether a *missing* target is an error: `status`/`sync` require
    // one, while `add` uses a configured target (if any) only as a safety guard.
    let target = if let Some(flag) = target_flag {
        Some(flag.to_string())
    } else if let Some(val) = file.target {
        Some(val)
    } else if need_target {
        return Err(anyhow!("--target is required (no target in config)"));
    } else {
        None
    };

    Ok(ResolvedConfig { home, repo, target })
}

/// Canonicalize an already-expanded path, applying the same checks that
/// [`crate::util::home_dir`] applies: must be absolute, then
/// [`std::fs::canonicalize`].
fn canonicalize_config_path(path: PathBuf, label: &str) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(anyhow!(
            "{label} must be an absolute path, got `{}`",
            path.display()
        ));
    }
    std::fs::canonicalize(&path)
        .with_context(|| format!("failed to canonicalize {label} at `{}`", path.display()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Global mutex to serialise tests that mutate process-wide env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // -- default_config_path ------------------------------------------------

    #[test]
    fn default_path_uses_xdg_config_home_when_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: single-threaded thanks to ENV_LOCK.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", "/tmp/my-xdg");
        }
        let path = default_config_path().unwrap();
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        assert_eq!(path, PathBuf::from("/tmp/my-xdg/dotmerge/config.toml"));
    }

    #[test]
    fn default_path_falls_back_to_home_config_when_xdg_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        let real_home = std::env::var_os("HOME").expect("$HOME must be set in tests");
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        let path = default_config_path().unwrap();
        assert_eq!(
            path,
            PathBuf::from(&real_home)
                .join(".config")
                .join("dotmerge")
                .join("config.toml")
        );
    }

    #[test]
    fn default_path_falls_back_when_xdg_is_empty() {
        let _guard = ENV_LOCK.lock().unwrap();
        let real_home = std::env::var_os("HOME").expect("$HOME must be set in tests");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", "");
        }
        let path = default_config_path().unwrap();
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        assert_eq!(
            path,
            PathBuf::from(&real_home)
                .join(".config")
                .join("dotmerge")
                .join("config.toml")
        );
    }

    // -- expand_path --------------------------------------------------------

    #[test]
    fn expand_path_tilde_uses_real_home() {
        let real_home = std::env::var("HOME").expect("$HOME must be set in tests");
        let result = expand_path("~/foo/bar").unwrap();
        assert_eq!(result, PathBuf::from(&real_home).join("foo/bar"));
    }

    #[test]
    fn expand_path_absolute_passes_through() {
        let result = expand_path("/usr/local/share").unwrap();
        assert_eq!(result, PathBuf::from("/usr/local/share"));
    }

    #[test]
    fn expand_path_relative_non_tilde_errors() {
        let err = expand_path("relative/path").unwrap_err();
        assert!(
            err.to_string()
                .contains("paths must be absolute or start with `~/`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn expand_path_bare_tilde_errors() {
        // A bare `~` (no trailing slash) is not the `~/` prefix — it's relative.
        let err = expand_path("~").unwrap_err();
        assert!(
            err.to_string()
                .contains("paths must be absolute or start with `~/`"),
            "unexpected error: {err}"
        );
    }

    // -- parsing ------------------------------------------------------------

    #[test]
    fn parse_rejects_unknown_fields() {
        let toml_text = r#"tagret = "origin/main""#;
        let result: Result<FileConfig, _> = toml::from_str(toml_text);
        assert!(result.is_err(), "expected error for unknown field");
    }

    #[test]
    fn parse_accepts_all_known_fields() {
        let toml_text = r#"
home   = "~/dotmerge-home"
repo   = "~/dotmerge-repo"
target = "origin/main"
"#;
        let cfg: FileConfig = toml::from_str(toml_text).unwrap();
        assert_eq!(cfg.home.as_deref(), Some("~/dotmerge-home"));
        assert_eq!(cfg.repo.as_deref(), Some("~/dotmerge-repo"));
        assert_eq!(cfg.target.as_deref(), Some("origin/main"));
    }

    #[test]
    fn parse_accepts_partial_config() {
        let toml_text = r#"repo = "~/dotmerge-repo""#;
        let cfg: FileConfig = toml::from_str(toml_text).unwrap();
        assert!(cfg.home.is_none());
        assert_eq!(cfg.repo.as_deref(), Some("~/dotmerge-repo"));
        assert!(cfg.target.is_none());
    }

    // -- load ---------------------------------------------------------------

    #[test]
    fn load_flag_missing_file_errors() {
        let path = PathBuf::from("/tmp/dotmerge-nonexistent-config-12345.toml");
        let err = load(Some(&path)).unwrap_err();
        assert!(
            err.to_string().contains("--config path does not exist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_env_missing_file_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var(
                "DOTMERGE_CONFIG",
                "/tmp/dotmerge-nonexistent-config-67890.toml",
            );
        }
        let err = load(None).unwrap_err();
        unsafe {
            std::env::remove_var("DOTMERGE_CONFIG");
        }
        assert!(
            err.to_string()
                .contains("DOTMERGE_CONFIG path does not exist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn load_default_missing_yields_empty_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Point $HOME and XDG at a dir we know has no config file.
        let tmp = TempDir::new().unwrap();
        unsafe {
            std::env::remove_var("DOTMERGE_CONFIG");
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }
        let cfg = load(None).unwrap();
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        assert!(cfg.home.is_none());
        assert!(cfg.repo.is_none());
        assert!(cfg.target.is_none());
    }

    #[test]
    fn load_flag_reads_file() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(&cfg_path, r#"target = "origin/main""#).unwrap();
        let cfg = load(Some(&cfg_path)).unwrap();
        assert_eq!(cfg.target.as_deref(), Some("origin/main"));
    }

    #[test]
    fn load_env_reads_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(&cfg_path, r#"repo = "~/myrepo""#).unwrap();
        unsafe {
            std::env::set_var("DOTMERGE_CONFIG", &cfg_path);
        }
        let cfg = load(None).unwrap();
        unsafe {
            std::env::remove_var("DOTMERGE_CONFIG");
        }
        assert_eq!(cfg.repo.as_deref(), Some("~/myrepo"));
    }

    #[test]
    fn load_flag_wins_over_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();

        let flag_path = tmp.path().join("flag.toml");
        std::fs::write(&flag_path, r#"target = "flag-target""#).unwrap();

        let env_path = tmp.path().join("env.toml");
        std::fs::write(&env_path, r#"target = "env-target""#).unwrap();

        unsafe {
            std::env::set_var("DOTMERGE_CONFIG", &env_path);
        }
        let cfg = load(Some(&flag_path)).unwrap();
        unsafe {
            std::env::remove_var("DOTMERGE_CONFIG");
        }
        assert_eq!(cfg.target.as_deref(), Some("flag-target"));
    }

    // -- resolve ------------------------------------------------------------

    /// target_flag wins over a config-file target.
    #[test]
    fn resolve_target_flag_wins_over_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();

        // Write a config with a target that should be overridden.
        let cfg_path = tmp.path().join("config.toml");
        // We also need repo so resolve doesn't fail on the repo ladder.
        // Use a real directory for repo so canonicalize succeeds.
        let repo_dir = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(
            &cfg_path,
            format!(
                "repo = \"{}\"\ntarget = \"config-target\"\n",
                repo_dir.display()
            ),
        )
        .unwrap();

        let resolved = resolve(Some(&cfg_path), None, None, Some("flag-target"), true).unwrap();
        assert_eq!(resolved.target.as_deref(), Some("flag-target"));
    }

    /// When neither flag nor config provides target and need_target is true, error.
    #[test]
    fn resolve_missing_target_errors_when_required() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        let repo_dir = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(&cfg_path, format!("repo = \"{}\"\n", repo_dir.display())).unwrap();

        let err = resolve(Some(&cfg_path), None, None, None, true).unwrap_err();
        assert!(
            err.to_string().contains("--target is required"),
            "unexpected error: {err}"
        );
    }

    /// When repo is absent from both flag and config, error.
    #[test]
    fn resolve_missing_repo_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        // Config with only target — no repo.
        std::fs::write(&cfg_path, "target = \"origin/main\"\n").unwrap();

        let err = resolve(Some(&cfg_path), None, None, None, false).unwrap_err();
        assert!(
            err.to_string().contains("--repo is required"),
            "unexpected error: {err}"
        );
    }

    /// need_target=false still returns a configured target (used as a guard by
    /// `add`); only a *missing* target is tolerated.
    #[test]
    fn resolve_need_target_false_reads_config_target() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        let repo_dir = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(
            &cfg_path,
            format!(
                "repo = \"{}\"\ntarget = \"origin/main\"\n",
                repo_dir.display()
            ),
        )
        .unwrap();

        // No target_flag, need_target=false → config target is still returned.
        let resolved = resolve(Some(&cfg_path), None, None, None, false).unwrap();
        assert_eq!(resolved.target.as_deref(), Some("origin/main"));

        // ...but a missing target is tolerated (no error) for need_target=false.
        std::fs::write(&cfg_path, format!("repo = \"{}\"\n", repo_dir.display())).unwrap();
        let resolved = resolve(Some(&cfg_path), None, None, None, false).unwrap();
        assert!(resolved.target.is_none());
    }
}
