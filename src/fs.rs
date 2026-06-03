use crate::model::{AddSourceKind, ValidatedAddSource};
use anyhow::{Context, Result, anyhow};
use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

pub fn validate_add_source(input_path: &Path, home: &Path) -> Result<ValidatedAddSource> {
    let source_path = resolve_home_path(input_path, home)?;
    let metadata = fs::symlink_metadata(&source_path)
        .with_context(|| format!("failed to inspect source path `{}`", source_path.display()))
        .map_err(|err| match err.downcast::<std::io::Error>() {
            Ok(io_err) if io_err.kind() == ErrorKind::NotFound => {
                anyhow!("source path `{}` does not exist", source_path.display())
            }
            Ok(io_err) => anyhow!(io_err),
            Err(err) => err,
        })?;

    ensure_source_resolves_inside_home(&source_path, home, metadata.file_type().is_symlink())?;

    let kind = if metadata.file_type().is_file() {
        AddSourceKind::File {
            mode: metadata.permissions().mode(),
        }
    } else if metadata.file_type().is_symlink() {
        let target = fs::read_link(&source_path)
            .with_context(|| format!("failed to read symlink `{}`", source_path.display()))?;
        AddSourceKind::Symlink { target }
    } else if metadata.file_type().is_dir() {
        return Err(anyhow!(
            "source path `{}` is a directory; only regular files and symlinks can be added",
            source_path.display()
        ));
    } else {
        return Err(anyhow!(
            "source path `{}` is not a regular file or symlink",
            source_path.display()
        ));
    };

    let repo_path = source_path
        .strip_prefix(home)
        .map(Path::to_path_buf)
        .map_err(|_| {
            anyhow!(
                "source path `{}` does not map to a path inside `$HOME` `{}`",
                source_path.display(),
                home.display()
            )
        })?;

    Ok(ValidatedAddSource {
        input_path: input_path.to_path_buf(),
        source_path,
        repo_path,
        kind,
    })
}

pub fn ensure_working_copy_target_available(
    repo_root: &Path,
    repo_relative: &Path,
    destination_path: &Path,
) -> Result<()> {
    match fs::symlink_metadata(destination_path) {
        Ok(_) => {
            return Err(anyhow!(
                "repo working copy already contains `{}` at `{}`",
                repo_relative.display(),
                destination_path.display()
            ));
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => {
            return Err(anyhow!(err)).with_context(|| {
                format!(
                    "failed to inspect repo working-copy path `{}`",
                    destination_path.display()
                )
            });
        }
    }

    let mut current = destination_path.parent().ok_or_else(|| {
        anyhow!(
            "destination path `{}` has no parent",
            destination_path.display()
        )
    })?;
    while current != repo_root {
        match fs::symlink_metadata(current) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(anyhow!(
                    "cannot add `{}` because parent path `{}` exists in the repo working copy and is not a directory",
                    repo_relative.display(),
                    current.display()
                ));
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(anyhow!(err)).with_context(|| {
                    format!("failed to inspect parent path `{}`", current.display())
                });
            }
        }

        current = current.parent().ok_or_else(|| {
            anyhow!(
                "destination path `{}` escapes the repo working copy",
                destination_path.display()
            )
        })?;
    }

    Ok(())
}

pub fn copy_add_source(source: &ValidatedAddSource, destination_path: &Path) -> Result<()> {
    let parent = destination_path.parent().ok_or_else(|| {
        anyhow!(
            "destination path `{}` has no parent",
            destination_path.display()
        )
    })?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create parent directory `{}`", parent.display()))?;

    match &source.kind {
        AddSourceKind::File { mode } => {
            fs::copy(&source.source_path, destination_path).with_context(|| {
                format!(
                    "failed to copy `{}` to `{}`",
                    source.source_path.display(),
                    destination_path.display()
                )
            })?;
            fs::set_permissions(destination_path, fs::Permissions::from_mode(*mode)).with_context(
                || {
                    format!(
                        "failed to set permissions on `{}`",
                        destination_path.display()
                    )
                },
            )?;
        }
        AddSourceKind::Symlink { target } => {
            symlink(target, destination_path).with_context(|| {
                format!(
                    "failed to create symlink `{}` -> `{}`",
                    destination_path.display(),
                    target.display()
                )
            })?;
        }
    }

    Ok(())
}

fn resolve_home_path(input_path: &Path, home: &Path) -> Result<PathBuf> {
    let expanded = if input_path.is_absolute() {
        input_path.to_path_buf()
    } else if let Ok(stripped) = input_path.strip_prefix("~") {
        home.join(stripped)
    } else {
        home.join(input_path)
    };

    let normalized = normalize_absolute_path(&expanded)?;
    if normalized.starts_with(home) {
        Ok(normalized)
    } else {
        Err(anyhow!(
            "input path `{}` resolves outside `$HOME` `{}`",
            input_path.display(),
            home.display()
        ))
    }
}

fn ensure_source_resolves_inside_home(
    source_path: &Path,
    home: &Path,
    is_symlink: bool,
) -> Result<()> {
    let resolved = if is_symlink {
        let parent = source_path.parent().ok_or_else(|| {
            anyhow!(
                "source path `{}` has no parent directory",
                source_path.display()
            )
        })?;
        let file_name = source_path
            .file_name()
            .ok_or_else(|| anyhow!("source path `{}` has no file name", source_path.display()))?;
        fs::canonicalize(parent)
            .with_context(|| format!("failed to resolve parent directory `{}`", parent.display()))?
            .join(file_name)
    } else {
        fs::canonicalize(source_path)
            .with_context(|| format!("failed to resolve source path `{}`", source_path.display()))?
    };

    if resolved.starts_with(home) {
        Ok(())
    } else {
        Err(anyhow!(
            "source path `{}` resolves outside `$HOME` via `{}`",
            source_path.display(),
            resolved.display()
        ))
    }
}

fn normalize_absolute_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(anyhow!(
            "expected an absolute path, got `{}`",
            path.display()
        ));
    }

    let mut prefix: Option<OsString> = None;
    let mut has_root = false;
    let mut parts: Vec<OsString> = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(value) => prefix = Some(value.as_os_str().to_os_string()),
            Component::RootDir => has_root = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if !parts.is_empty() {
                    parts.pop();
                }
            }
            Component::Normal(value) => parts.push(value.to_os_string()),
        }
    }

    let mut normalized = PathBuf::new();
    if let Some(prefix) = prefix {
        normalized.push(prefix);
    }
    if has_root {
        normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR));
    }
    for part in parts {
        normalized.push(part);
    }

    Ok(normalized)
}
