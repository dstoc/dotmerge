use crate::cli::SyncArgs;
use crate::fs;
use crate::jj::JjClient;
use crate::model::RevisionSummary;
use crate::status;
use crate::util;
use anyhow::{Result, anyhow};
use std::path::Path;

pub fn run(args: SyncArgs) -> Result<()> {
    let home = util::home_dir()?;
    let client = JjClient::open(&args.common.repo)?;
    let target = client.resolve_rev(&args.common.target)?;

    if !client.is_working_copy_clean()? {
        return Err(anyhow!(
            "repo working copy is not clean; refusing to start sync until the repo state is unambiguous"
        ));
    }

    let last_sync = client.bookmark_summary("last-sync")?;
    let current_import = client.bookmark_summary("current-import")?;
    let base = match &last_sync.revision {
        Some(revision) => revision.clone(),
        None => client.root_revision()?,
    };

    validate_resume_state(&client, &base, &target, current_import.revision.as_ref())?;

    let managed_paths = status::managed_paths(&client, &base, &target)?;
    let imported = client.create_or_refresh_import(&base, &home, &managed_paths)?;
    let merged = client.merge_revisions(&imported, &target)?;
    client.checkout_revision(&merged)?;

    if client.has_conflicts(&merged)? {
        return Err(anyhow!(
            "merge produced jj conflicts at `@`; resolve them in the repo, then rerun `dotmerge sync`"
        ));
    }

    if args.no_export {
        let summary = status::collect(&client, &home, target)?;
        status::print_summary(&summary);
        return Ok(());
    }

    export_revision_to_home(&client, &home, &merged, &managed_paths)?;
    client.complete_sync(&merged)?;

    let summary = status::collect(&client, &home, target)?;
    status::print_summary(&summary);
    Ok(())
}

fn validate_resume_state(
    client: &JjClient,
    base: &RevisionSummary,
    target: &RevisionSummary,
    current_import: Option<&RevisionSummary>,
) -> Result<()> {
    let Some(current_import) = current_import else {
        return Ok(());
    };

    let current = client.current_revision()?;
    if !client.is_ancestor(base, current_import)? {
        return Err(anyhow!(
            "`current-import` is not descended from `last-sync`; refusing to rewrite sync state"
        ));
    }
    if !client.is_ancestor(current_import, &current)? {
        return Err(anyhow!(
            "`current-import` is not an ancestor of the current `@` revision; manual repair is required before sync can continue"
        ));
    }
    if client.is_ancestor(current_import, target)? {
        return Err(anyhow!(
            "`current-import` is already an ancestor of the requested target; refusing to guess how to resume"
        ));
    }

    Ok(())
}

fn export_revision_to_home(
    client: &JjClient,
    home: &Path,
    revision: &RevisionSummary,
    managed_paths: &std::collections::BTreeSet<std::path::PathBuf>,
) -> Result<()> {
    let entries = client.read_entries_at_rev(revision, managed_paths)?;
    let mut export_entries = Vec::new();
    for (path, entry) in entries {
        if let Some(entry) = entry {
            export_entries.push((path, entry));
        }
    }
    fs::export_home_entries(home, &export_entries)?;
    Ok(())
}
