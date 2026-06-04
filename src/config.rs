// This module is wired into the rest of the binary in Steps 2-3; suppress
// "unused" lints that fire because the items are not yet called from main.
#![allow(dead_code)]

use anyhow::{anyhow, Context, Result};
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
        let home = std::env::var_os("HOME")
            .ok_or_else(|| anyhow!("$HOME is not set"))?;
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
        let home = std::env::var_os("HOME")
            .ok_or_else(|| anyhow!("$HOME is not set"))?;
        return Ok(PathBuf::from(home).join(rest));
    }
    Err(anyhow!(
        "paths must be absolute or start with `~/`, got `{raw}`"
    ))
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
            err.to_string().contains("DOTMERGE_CONFIG path does not exist"),
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
}
