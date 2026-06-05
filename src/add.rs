use crate::cli::AddArgs;
use crate::config;
use crate::fs;
use crate::jj::JjClient;
use crate::model::ValidatedAddSource;
use anyhow::{anyhow, Result};
use std::collections::HashSet;
use std::path::PathBuf;

struct PlannedAdd {
    source: ValidatedAddSource,
    destination_path: PathBuf,
}

pub fn run(config_flag: Option<&std::path::Path>, args: AddArgs) -> Result<()> {
    let resolved = config::resolve(
        config_flag,
        args.home.as_deref(),
        args.repo.as_deref(),
        None,
        false,
    )?;
    let home = resolved.home;
    let client = JjClient::open(&resolved.repo)?;

    ensure_addable_working_copy(&client, resolved.target.as_deref())?;

    let mut seen_repo_paths = HashSet::new();
    let mut planned = Vec::with_capacity(args.paths.len());

    for input_path in &args.paths {
        let source = fs::validate_add_source(input_path, &home)?;
        if !seen_repo_paths.insert(source.repo_path.clone()) {
            return Err(anyhow!(
                "multiple inputs map to the same repo path `{}`",
                source.repo_path.display()
            ));
        }
        ensure_no_batch_path_conflicts(&planned, &source)?;

        let destination_path = client.working_copy_path(&source.repo_path)?;
        fs::ensure_working_copy_target_available(
            client.repo_path(),
            &source.repo_path,
            &destination_path,
        )?;

        planned.push(PlannedAdd {
            source,
            destination_path,
        });
    }

    for entry in &planned {
        fs::copy_add_source(&entry.source, &entry.destination_path)?;
    }

    Ok(())
}

/// Refuse to add when `@` coincides with a sync-critical revision.
///
/// `add` writes directly into the `@` working copy, so it must sit on a fresh
/// change — not on `last-sync` (or an ancestor of it), the configured target
/// (or an ancestor of it), or `current-import`. Adding onto any of those would
/// rewrite synced/target history or pollute the in-progress import. The fix is
/// always to start a fresh change with `jj new` first.
fn ensure_addable_working_copy(client: &JjClient, target: Option<&str>) -> Result<()> {
    let session = client.begin()?;
    let current = session.current_revision()?;

    if let Some(last_sync) = session.bookmark_summary("last-sync")?.revision
        && session.is_ancestor(&current, &last_sync)?
    {
        return Err(anyhow!(
            "`@` is at or below `last-sync`, so `dotmerge add` would rewrite already-synced history.\n\nstart a fresh change first (`jj new`), then rerun `dotmerge add`."
        ));
    }

    if let Some(current_import) = session.bookmark_summary("current-import")?.revision
        && current.same(&current_import)
    {
        return Err(anyhow!(
            "`@` is `current-import`, so `dotmerge add` would pollute the in-progress import.\n\nstart a fresh change first (`jj new`), then rerun `dotmerge add`."
        ));
    }

    if let Some(target) = target {
        // The configured target may not resolve yet (e.g. during bootstrap
        // before it exists); a target we can't resolve simply isn't checked.
        if let Ok(target_rev) = session.resolve_rev(target)
            && session.is_ancestor(&current, &target_rev)?
        {
            return Err(anyhow!(
                "`@` is at or below the target `{target}`, so `dotmerge add` would rewrite target history.\n\nstart a fresh change first (`jj new`), then rerun `dotmerge add`."
            ));
        }
    }

    Ok(())
}

fn ensure_no_batch_path_conflicts(
    planned: &[PlannedAdd],
    candidate: &ValidatedAddSource,
) -> Result<()> {
    for existing in planned {
        if candidate.repo_path.starts_with(&existing.source.repo_path) {
            return Err(anyhow!(
                "cannot add both `{}` and `{}` because `{}` would need to be both a file path and a parent directory",
                existing.source.input_path.display(),
                candidate.input_path.display(),
                existing.source.repo_path.display()
            ));
        }
        if existing.source.repo_path.starts_with(&candidate.repo_path) {
            return Err(anyhow!(
                "cannot add both `{}` and `{}` because `{}` would need to be both a file path and a parent directory",
                existing.source.input_path.display(),
                candidate.input_path.display(),
                candidate.repo_path.display()
            ));
        }
    }

    Ok(())
}
