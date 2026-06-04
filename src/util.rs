use anyhow::{Context, anyhow};
use std::path::PathBuf;

pub fn not_implemented(feature: &str) -> anyhow::Error {
    anyhow!("{feature} not implemented yet")
}

pub fn hostname_label() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown host".to_string())
}

pub fn home_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("$HOME is not set"))?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err(anyhow!(
            "$HOME must be an absolute path, got `{}`",
            home.display()
        ));
    }

    std::fs::canonicalize(&home)
        .with_context(|| format!("failed to canonicalize `$HOME` at `{}`", home.display()))
}
