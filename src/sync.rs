use crate::cli::SyncArgs;
use crate::config;
use crate::export;
use crate::import;
use crate::jj::{JjClient, JjSession};
use crate::merge;
use crate::model::{BookmarkSummary, FileStatusSummary, MergeOutcome, ResumeState, Revision};
use crate::status;
use crate::util;
use anyhow::{anyhow, Result};

pub fn run(config_flag: Option<&std::path::Path>, args: SyncArgs) -> Result<()> {
    let resolved = config::resolve(
        config_flag,
        args.common.home.as_deref(),
        args.common.repo.as_deref(),
        args.common.target.as_deref(),
        true,
    )?;
    let home = resolved.home;
    let client = JjClient::open(&resolved.repo)?;
    let repo_path = client.repo_path().to_path_buf();
    let mut session = client.begin()?;
    let target_str = resolved.target.expect("need_target=true guarantees Some");
    let target = session.resolve_rev(&target_str)?;

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

    validate_resume_state(&session, &base, current_import.revision.as_ref())?;

    let managed_paths = status::managed_paths(&session, &target)?;
    let imported = import::create_or_refresh_import(&mut session, &base, &home, &managed_paths)?;
    let merged = merge::merge_revisions(&mut session, &imported.revision, &target)?;
    let current = session.current_revision()?;
    if !merged.revision().same(&current) {
        session.checkout_revision(merged.revision())?;
    }

    if session.has_conflicts(merged.revision())? {
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

    let exported =
        export::export_revision_to_home(&session, &home, merged.revision(), &managed_paths)?;
    session.complete_sync(merged.revision())?;

    // Build the recap before finish() consumes the session (target_label needs it).
    let recap = build_recap(&session, &last_sync, &imported.imported, &merged, &exported, &target)?;
    session.finish("dotmerge sync")?;
    print!("{recap}");
    Ok(())
}

fn validate_resume_state(
    session: &impl status::StatusSource,
    base: &Revision,
    current_import: Option<&Revision>,
) -> Result<()> {
    match session.resume_state(base, current_import)? {
        ResumeState::Fresh | ResumeState::Resumable => Ok(()),
        ResumeState::Blocked { reason } => Err(anyhow!(reason)),
    }
}

/// Build the past-tense recap string for a successful full sync.
///
/// Must be called before `session.finish()` because `merge::target_label`
/// needs to inspect the session's repo view.
fn build_recap(
    session: &JjSession,
    old_last_sync: &BookmarkSummary,
    imported: &[FileStatusSummary],
    merged: &MergeOutcome,
    exported: &[FileStatusSummary],
    target: &Revision,
) -> Result<String> {
    let old_id = old_last_sync
        .revision
        .as_ref()
        .map(|r| r.short_id())
        .unwrap_or_else(|| "(none)".to_string());
    let new_id = merged.revision().short_id();

    // Whole no-op: nothing imported, no real merge, nothing exported.
    if imported.is_empty() && !matches!(merged, MergeOutcome::Merged { .. }) && exported.is_empty()
    {
        return Ok(format!(
            "synced: already up to date — $HOME, repo, and target agree (last-sync {new_id})\n"
        ));
    }

    let mut out = String::new();
    out.push_str(&format!("synced: last-sync {old_id} → {new_id}\n"));
    out.push('\n');

    // Column label width: "exported to $HOME" = 17 chars; pad to 22.
    const LABEL_WIDTH: usize = 22;

    // imported from $HOME row
    let import_label = "imported from $HOME";
    if imported.is_empty() {
        out.push_str(&format!(
            "{:<LABEL_WIDTH$}nothing — $HOME matched last-sync\n",
            import_label
        ));
    } else {
        let detail = format_file_detail(imported);
        let count = imported.len();
        out.push_str(&format!(
            "{:<LABEL_WIDTH$}{count} {}   ({detail})\n",
            import_label,
            pluralize_file(count),
        ));
    }

    // merge row
    let merge_label = "merge";
    let merge_desc = format_merge_outcome(session, merged, target)?;
    out.push_str(&format!("{:<LABEL_WIDTH$}{merge_desc}\n", merge_label));

    // exported to $HOME row
    let export_label = "exported to $HOME";
    if exported.is_empty() {
        out.push_str(&format!(
            "{:<LABEL_WIDTH$}nothing — $HOME already matched\n",
            export_label
        ));
    } else {
        let detail = format_file_detail(exported);
        let count = exported.len();
        out.push_str(&format!(
            "{:<LABEL_WIDTH$}{count} {}   ({detail})\n",
            export_label,
            pluralize_file(count),
        ));
    }

    Ok(out)
}

fn pluralize_file(count: usize) -> &'static str {
    if count == 1 { "file" } else { "files" }
}

/// Format a list of `FileStatusSummary` items as `<path> <kind>`, joined by
/// `, `, capped at 6 with `… N more` when longer.
fn format_file_detail(files: &[FileStatusSummary]) -> String {
    const CAP: usize = 6;
    let items: Vec<String> = files
        .iter()
        .take(CAP)
        .map(|f| {
            format!(
                "{} {}",
                f.path.display(),
                status::kind_label(&f.kind)
            )
        })
        .collect();
    let mut result = items.join(", ");
    if files.len() > CAP {
        result.push_str(&format!(", … {} more", files.len() - CAP));
    }
    result
}

fn format_merge_outcome(
    session: &JjSession,
    merged: &MergeOutcome,
    target: &Revision,
) -> Result<String> {
    Ok(match merged {
        MergeOutcome::NoOp { .. } => {
            "none (target already contained the import)".to_string()
        }
        MergeOutcome::FastForward { .. } => "fast-forward to target".to_string(),
        MergeOutcome::Merged { .. } => {
            let host = util::hostname_label();
            let label = merge::target_label(session, target)?;
            format!("created merge commit ({host} into {label})")
        }
    })
}
