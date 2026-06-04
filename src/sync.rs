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
    let repo_path = client.repo_path().to_path_buf();
    let mut session = client.begin()?;
    let target = session.resolve_rev(&args.common.target)?;

    if !status::is_working_copy_clean(&session, &repo_path)? {
        return Err(anyhow!(
            "repo working copy is not clean; refusing to start sync until the repo state is unambiguous"
        ));
    }

    let last_sync = session.bookmark_summary("last-sync")?;
    let current_import = session.bookmark_summary("current-import")?;
    let base = match &last_sync.revision {
        Some(revision) => revision.clone(),
        None => session.root_revision()?,
    };

    validate_resume_state(&base, &target, current_import.revision.as_ref())?;

    let managed_paths = status::managed_paths(&session, &target)?;
    let imported = session.create_or_refresh_import(&base, &home, &managed_paths)?;
    let merged = session.merge_revisions(&imported, &target)?;
    let current = session.current_revision()?;
    if !merged.same_revision(&current) {
        session.checkout_revision(&merged)?;
    }

    if session.has_conflicts(&merged)? {
        return Err(anyhow!(
            "merge produced jj conflicts at `@`; resolve them in the repo, then rerun `dotmerge sync`"
        ));
    }

    if args.no_export {
        let summary = status::collect_for_sync(&session, &repo_path, &home, target)?;
        session.finish("dotmerge sync")?;
        status::print_summary(&summary);
        return Ok(());
    }

    export_revision_to_home(&session, &home, &merged, &managed_paths)?;
    session.complete_sync(&merged)?;

    let summary = status::collect_for_sync(&session, &repo_path, &home, target)?;
    session.finish("dotmerge sync")?;
    status::print_summary(&summary);
    Ok(())
}

fn validate_resume_state(
    _base: &RevisionSummary,
    _target: &RevisionSummary,
    _current_import: Option<&RevisionSummary>,
) -> Result<()> {
    Ok(())
}

fn export_revision_to_home(
    session: &impl status::StatusSource,
    home: &Path,
    revision: &RevisionSummary,
    managed_paths: &std::collections::BTreeSet<std::path::PathBuf>,
) -> Result<()> {
    let entries = session.read_entries_at_rev(revision, managed_paths)?;
    let mut export_entries = Vec::new();
    for (path, entry) in entries {
        if let Some(entry) = entry {
            export_entries.push((path, entry));
        }
    }
    fs::export_home_entries(home, &export_entries)?;
    Ok(())
}
