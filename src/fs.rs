use crate::model::{AddSourceKind, ManagedEntry, ValidatedAddSource};
use anyhow::{anyhow, Context, Result};
use std::ffi::OsString;
use std::fs;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // normalize_absolute_path
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_collapses_dot_and_dotdot() -> Result<()> {
        let result = normalize_absolute_path(Path::new("/home/u/./a/../b"))?;
        assert_eq!(result, PathBuf::from("/home/u/b"));
        Ok(())
    }

    #[test]
    fn normalize_dotdot_at_root_stays_clamped() -> Result<()> {
        // Going above root should clamp at root, not escape it.
        let result = normalize_absolute_path(Path::new("/../../../etc/passwd"))?;
        assert_eq!(result, PathBuf::from("/etc/passwd"));
        Ok(())
    }

    #[test]
    fn normalize_rejects_relative_path() {
        let result = normalize_absolute_path(Path::new("relative/path"));
        assert!(result.is_err(), "expected error for relative path");
    }

    #[test]
    fn normalize_clean_absolute_path_unchanged() -> Result<()> {
        let result = normalize_absolute_path(Path::new("/a/b/c"))?;
        assert_eq!(result, PathBuf::from("/a/b/c"));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // resolve_home_path
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_absolute_path_inside_home_accepted() -> Result<()> {
        let tmp = TempDir::new()?;
        let home = tmp.path();
        let input = home.join("subdir/file.txt");
        let result = resolve_home_path(&input, home)?;
        assert!(result.starts_with(home));
        assert_eq!(result, home.join("subdir/file.txt"));
        Ok(())
    }

    #[test]
    fn resolve_tilde_prefix_expands_to_home() -> Result<()> {
        let tmp = TempDir::new()?;
        let home = tmp.path();
        let result = resolve_home_path(Path::new("~/x"), home)?;
        assert!(result.starts_with(home));
        assert_eq!(result, home.join("x"));
        Ok(())
    }

    #[test]
    fn resolve_bare_relative_path_resolves_under_home() -> Result<()> {
        let tmp = TempDir::new()?;
        let home = tmp.path();
        let result = resolve_home_path(Path::new("x"), home)?;
        assert!(result.starts_with(home));
        assert_eq!(result, home.join("x"));
        Ok(())
    }

    #[test]
    fn resolve_tilde_dotdot_outside_home_rejected() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        // ~/../outside lexically resolves to <home>/../outside which is outside home
        let result = resolve_home_path(Path::new("~/../outside"), home);
        assert!(result.is_err(), "expected rejection for path escaping home via ~/../");
    }

    #[test]
    fn resolve_absolute_path_outside_home_rejected() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        // Construct an absolute path that goes outside: <home>/../outside
        let outside = home.join("../outside");
        let result = resolve_home_path(&outside, home);
        assert!(result.is_err(), "expected rejection for absolute path outside home");
    }

    #[test]
    fn resolve_relative_dotdot_outside_home_rejected() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        // ../etc/passwd relative to home resolves outside
        let result = resolve_home_path(Path::new("../etc/passwd"), home);
        assert!(result.is_err(), "expected rejection for ../etc/passwd");
    }

    // -----------------------------------------------------------------------
    // ensure_source_resolves_inside_home
    // -----------------------------------------------------------------------

    #[test]
    fn ensure_regular_file_inside_home_accepted() -> Result<()> {
        let tmp = TempDir::new()?;
        let home = tmp.path();
        let file_path = home.join("myfile.txt");
        fs::write(&file_path, b"content")?;
        // is_symlink=false: uses fs::canonicalize on the file itself
        ensure_source_resolves_inside_home(&file_path, home, false)?;
        Ok(())
    }

    #[test]
    fn ensure_symlink_inside_home_pointing_outside_accepted() -> Result<()> {
        // The function checks that the *symlink itself* (its location) is inside home,
        // not where the symlink points. A symlink inside home whose target is outside
        // home is accepted — the target path is stored separately, not validated here.
        let tmp = TempDir::new()?;
        let home = tmp.path();

        // Create a real file outside of home (one level up in a sibling dir)
        let outside_dir = tmp.path().parent().unwrap().join("dotmerge-test-outside");
        fs::create_dir_all(&outside_dir)?;
        let outside_file = outside_dir.join("secret.txt");
        fs::write(&outside_file, b"secret")?;

        // Create a symlink inside home pointing to the outside file
        let link_path = home.join("escape_link");
        symlink(&outside_file, &link_path)?;

        // is_symlink=true: only canonicalizes the *parent*, then re-joins the filename.
        // The symlink's location is inside home, so this must succeed.
        ensure_source_resolves_inside_home(&link_path, home, true)?;

        let _ = fs::remove_dir_all(&outside_dir);
        Ok(())
    }

    #[test]
    fn ensure_symlink_inside_home_pointing_inside_accepted() -> Result<()> {
        let tmp = TempDir::new()?;
        let home = tmp.path();

        // Create a real file inside home
        let target_file = home.join("target.txt");
        fs::write(&target_file, b"content")?;

        // Create a symlink inside home pointing to the target file inside home
        let link_path = home.join("link_to_target");
        symlink(&target_file, &link_path)?;

        // is_symlink=true: the symlink's parent (home) is canonicalized, symlink itself is not followed
        ensure_source_resolves_inside_home(&link_path, home, true)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // validate_add_source (end-to-end)
    // -----------------------------------------------------------------------

    #[test]
    fn validate_regular_file_returns_correct_repo_path_and_kind() -> Result<()> {
        let tmp = TempDir::new()?;
        let home = tmp.path();
        let file_path = home.join("config/settings.toml");
        fs::create_dir_all(file_path.parent().unwrap())?;
        fs::write(&file_path, b"[settings]")?;

        let result = validate_add_source(&file_path, home)?;

        assert_eq!(result.repo_path, PathBuf::from("config/settings.toml"));
        assert!(
            matches!(result.kind, AddSourceKind::File { .. }),
            "expected File kind"
        );
        Ok(())
    }

    #[test]
    fn validate_path_outside_home_rejected() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        // Use an absolute path that is a parent of home — definitely outside
        let outside = home.parent().unwrap().to_path_buf();
        let result = validate_add_source(&outside, home);
        assert!(result.is_err(), "expected rejection for path outside home");
    }

    #[test]
    fn validate_directory_rejected() -> Result<()> {
        let tmp = TempDir::new()?;
        let home = tmp.path();
        let dir_path = home.join("mydir");
        fs::create_dir_all(&dir_path)?;

        let result = validate_add_source(&dir_path, home);
        assert!(result.is_err(), "expected rejection for directory input");
        Ok(())
    }

    #[test]
    fn validate_symlink_escaping_home_is_accepted_target_stored() -> Result<()> {
        // validate_add_source accepts a symlink inside home whose target is outside.
        // The design stores the target path verbatim in AddSourceKind::Symlink — it does
        // not restrict where a symlink may point, only that the symlink file itself lives
        // under home.
        let tmp = TempDir::new()?;
        let home = tmp.path();

        let outside_dir = tmp.path().parent().unwrap().join("dotmerge-test-outside2");
        fs::create_dir_all(&outside_dir)?;
        let outside_file = outside_dir.join("secret2.txt");
        fs::write(&outside_file, b"secret")?;

        let link_path = home.join("external_link");
        symlink(&outside_file, &link_path)?;

        let result = validate_add_source(&link_path, home)?;
        assert_eq!(result.repo_path, PathBuf::from("external_link"));
        assert!(
            matches!(result.kind, AddSourceKind::Symlink { ref target } if target == &outside_file),
            "expected Symlink kind with the outside target stored verbatim"
        );

        let _ = fs::remove_dir_all(&outside_dir);
        Ok(())
    }

    #[test]
    fn validate_symlink_inside_home_returns_symlink_kind() -> Result<()> {
        let tmp = TempDir::new()?;
        let home = tmp.path();

        let target_file = home.join("dotfile");
        fs::write(&target_file, b"data")?;

        let link_path = home.join("link_to_dotfile");
        symlink(&target_file, &link_path)?;

        let result = validate_add_source(&link_path, home)?;
        assert_eq!(result.repo_path, PathBuf::from("link_to_dotfile"));
        assert!(
            matches!(result.kind, AddSourceKind::Symlink { .. }),
            "expected Symlink kind"
        );
        Ok(())
    }
}
