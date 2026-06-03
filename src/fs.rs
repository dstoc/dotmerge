use crate::model::{AddSourceKind, ManagedEntry, ValidatedAddSource};
use anyhow::{Context, Result, anyhow};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

pub fn list_repo_paths(repo_root: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut paths = BTreeSet::new();
    collect_repo_paths(repo_root, repo_root, &mut paths)?;
    Ok(paths)
}

pub fn read_rooted_entry(root: &Path, repo_relative: &Path) -> Result<Option<ManagedEntry>> {
    let path = root.join(repo_relative);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(Some(ManagedEntry::File {
            contents: fs::read(&path)
                .with_context(|| format!("failed to read file `{}`", path.display()))?,
            executable: metadata.permissions().mode() & 0o111 != 0,
        })),
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(Some(ManagedEntry::Symlink {
            target: fs::read_link(&path)
                .with_context(|| format!("failed to read symlink `{}`", path.display()))?,
        })),
        Ok(metadata) if metadata.file_type().is_dir() => Ok(Some(ManagedEntry::Unsupported {
            kind: "directory".to_string(),
        })),
        Ok(_) => Ok(Some(ManagedEntry::Unsupported {
            kind: "unsupported filesystem entry".to_string(),
        })),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow!(err))
            .with_context(|| format!("failed to inspect path `{}`", path.display())),
    }
}

pub fn export_home_entries(
    home: &Path,
    entries: &[(PathBuf, ManagedEntry)],
) -> Result<Vec<PathBuf>> {
    let mut exported = Vec::with_capacity(entries.len());

    for (repo_relative, entry) in entries {
        let destination = home.join(repo_relative);
        let parent = destination.parent().ok_or_else(|| {
            anyhow!(
                "destination path `{}` has no parent directory",
                destination.display()
            )
        })?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent directory `{}`", parent.display()))?;

        match entry {
            ManagedEntry::File {
                contents,
                executable,
            } => write_atomic_file(&destination, contents, *executable)?,
            ManagedEntry::Symlink { target } => write_atomic_symlink(&destination, target)?,
            ManagedEntry::Conflict => {
                return Err(anyhow!(
                    "refusing to export unresolved conflict at `{}`",
                    repo_relative.display()
                ));
            }
            ManagedEntry::Unsupported { kind } => {
                return Err(anyhow!(
                    "refusing to export unsupported `{kind}` entry at `{}`",
                    repo_relative.display()
                ));
            }
        }

        exported.push(repo_relative.clone());
    }

    Ok(exported)
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

fn collect_repo_paths(root: &Path, current: &Path, paths: &mut BTreeSet<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(current)
        .with_context(|| format!("failed to read directory `{}`", current.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry under `{}`", current.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to read file type for `{}`", path.display()))?;

        if current == root && (entry.file_name() == ".jj" || entry.file_name() == ".git") {
            continue;
        }

        if file_type.is_dir() {
            collect_repo_paths(root, &path, paths)?;
        } else if file_type.is_file() || file_type.is_symlink() {
            let relative = path
                .strip_prefix(root)
                .map(Path::to_path_buf)
                .map_err(|_| {
                    anyhow!(
                        "path `{}` escaped repo root `{}`",
                        path.display(),
                        root.display()
                    )
                })?;
            paths.insert(relative);
        }
    }

    Ok(())
}

fn write_atomic_file(destination: &Path, contents: &[u8], executable: bool) -> Result<()> {
    if destination
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(anyhow!(
            "cannot overwrite directory `{}` during export",
            destination.display()
        ));
    }

    let temp_path = unique_temp_path(destination)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .with_context(|| format!("failed to create temp file `{}`", temp_path.display()))?;
    file.write_all(contents)
        .with_context(|| format!("failed to write temp file `{}`", temp_path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync temp file `{}`", temp_path.display()))?;

    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(&temp_path, fs::Permissions::from_mode(mode)).with_context(|| {
        format!(
            "failed to set permissions on temp file `{}`",
            temp_path.display()
        )
    })?;

    fs::rename(&temp_path, destination).with_context(|| {
        format!(
            "failed to replace `{}` with `{}`",
            destination.display(),
            temp_path.display()
        )
    })?;
    Ok(())
}

fn write_atomic_symlink(destination: &Path, target: &Path) -> Result<()> {
    if destination
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(anyhow!(
            "cannot overwrite directory `{}` during export",
            destination.display()
        ));
    }

    let temp_path = unique_temp_path(destination)?;
    symlink(target, &temp_path).with_context(|| {
        format!(
            "failed to create temp symlink `{}` -> `{}`",
            temp_path.display(),
            target.display()
        )
    })?;
    fs::rename(&temp_path, destination).with_context(|| {
        format!(
            "failed to replace `{}` with `{}`",
            destination.display(),
            temp_path.display()
        )
    })?;
    Ok(())
}

fn unique_temp_path(destination: &Path) -> Result<PathBuf> {
    let parent = destination.parent().ok_or_else(|| {
        anyhow!(
            "destination path `{}` has no parent directory",
            destination.display()
        )
    })?;
    let file_name = destination.file_name().ok_or_else(|| {
        anyhow!(
            "destination path `{}` has no file name",
            destination.display()
        )
    })?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(
        ".dotmerge-{}.{}.tmp",
        file_name.to_string_lossy(),
        nanos
    )))
}
