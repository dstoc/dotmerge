use crate::cli::StatusArgs;
use crate::fs;
use crate::jj::JjClient;
use crate::model::{
    FileChangeKind, FileStatusSummary, ManagedEntry, RevisionSummary, SyncStatusSummary,
};
use crate::util;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub fn run(args: StatusArgs) -> Result<()> {
    let home = util::home_dir()?;
    let client = JjClient::open(&args.common.repo)?;
    let target = client.resolve_rev(&args.common.target)?;
    let summary = collect(&client, &home, target)?;
    print_summary(&summary);
    Ok(())
}

pub fn collect(
    client: &JjClient,
    home: &Path,
    target: RevisionSummary,
) -> Result<SyncStatusSummary> {
    let last_sync = client.bookmark_summary("last-sync")?;
    let current_import = client.bookmark_summary("current-import")?;
    let base = match &last_sync.revision {
        Some(revision) => revision.clone(),
        None => client.root_revision()?,
    };
    let current = client.current_revision()?;
    let mut summary = SyncStatusSummary::new(
        base.clone(),
        current_import.clone(),
        target.clone(),
        client.repo_path().to_path_buf(),
    );
    let target_already_applied = client.is_ancestor(&target, &current)?;
    summary.target_already_applied = target_already_applied;
    let managed_paths = managed_paths(client, &target)?;
    let base_entries = client.read_entries_at_rev(&base, &managed_paths)?;
    let target_entries = client.read_entries_at_rev(&target, &managed_paths)?;

    summary.repo_clean = Some(client.is_working_copy_clean()?);
    if !last_sync.exists {
        summary
            .notes
            .push("`last-sync` is missing; sync will use the empty tree as base.".to_string());
    }

    for path in &managed_paths {
        let home_entry = fs::read_rooted_entry(home, path)?;
        let base_entry = base_entries.get(path).cloned().flatten();
        let target_entry = target_entries.get(path).cloned().flatten();

        let home_kind = classify_change(base_entry.as_ref(), home_entry.as_ref());
        if home_kind != FileChangeKind::Unchanged {
            summary
                .home_changes
                .push(FileStatusSummary::new(path.clone(), home_kind.clone()));
            if matches!(home_kind, FileChangeKind::Deleted) {
                summary.deletion_candidates.push(path.clone());
            }
        }

        if !target_already_applied {
            let target_kind = classify_change(base_entry.as_ref(), target_entry.as_ref());
            if target_kind != FileChangeKind::Unchanged {
                summary
                    .target_changes
                    .push(FileStatusSummary::new(path.clone(), target_kind.clone()));
                if matches!(target_kind, FileChangeKind::Deleted)
                    && !summary.deletion_candidates.contains(path)
                {
                    summary.deletion_candidates.push(path.clone());
                }
            }
        }
    }

    summary.home_differs_from_base = !summary.home_changes.is_empty();
    summary.target_differs_from_base =
        !target_already_applied && !summary.target_changes.is_empty();

    if let Some(import_revision) = current_import.revision.as_ref() {
        if current.same_revision(import_revision) {
            summary.notes.push(
                "`current-import` already matches `@` and will be refreshed on sync.".to_string(),
            );
        } else {
            summary.prepared = Some(current.clone());
            summary
                .notes
                .push("sync will replace `current-import` with a direct child of the current repo-side `@` state before refreshing it.".to_string());
        }
    }

    summary.has_conflicts = summary.prepared.is_some() && client.has_conflicts(&current)?;
    if summary.has_conflicts {
        summary.notes.push(
            "jj conflicts are present at `@`; resolve them in the repo before export.".to_string(),
        );
    }
    if summary.repo_clean == Some(false) {
        summary
            .notes
            .push("repo working copy is not clean; sync will refuse to guess.".to_string());
    }
    if !summary.deletion_candidates.is_empty() {
        summary
            .notes
            .push("deletion candidates are present; MVP sync will only report them, not remove files from `$HOME`.".to_string());
    }
    if summary.repo_clean != Some(true) {
        summary
            .next_actions
            .push("repair the repo state until the working copy is clean".to_string());
    } else if summary.has_conflicts {
        summary
            .next_actions
            .push("resolve the jj conflicts at `@`, then rerun `dotmerge sync`".to_string());
    } else if !summary.home_differs_from_base
        && !summary.target_differs_from_base
        && summary.current_import.revision.is_none()
    {
        summary
            .next_actions
            .push("sync would leave the repo and `$HOME` unchanged".to_string());
    } else {
        summary
            .next_actions
            .push("refresh `current-import` from the current managed `$HOME` state".to_string());
        summary
            .next_actions
            .push("merge the imported state with the requested target revision".to_string());
        summary.next_actions.push(
            "export the merged files back to `$HOME` without deleting deletion candidates"
                .to_string(),
        );
    }

    Ok(summary)
}

pub fn print_summary(summary: &SyncStatusSummary) {
    println!("base:   last-sync = {}", summary.base.short_id());
    match &summary.current_import.revision {
        Some(revision) => println!("import: current-import = {}", revision.short_id()),
        None => println!("import: current-import = missing"),
    }
    if let Some(prepared) = &summary.prepared {
        let suffix = if summary.has_conflicts {
            " (prepared, conflicts)"
        } else {
            " (prepared)"
        };
        println!("merge:  @ = {}{}", prepared.short_id(), suffix);
    }
    println!(
        "target: {} = {}",
        summary.target.expression,
        summary.target.short_id()
    );
    println!("repo:   {}", summary.repo_path.display());
    println!();

    println!(
        "home:   {}",
        describe_relation(
            summary.home_differs_from_base,
            summary.home_changes.len(),
            "change"
        )
    );
    print_changes("home changes since base", &summary.home_changes);
    if summary.target_already_applied {
        println!("target: already applied");
    } else {
        println!(
            "target: {}",
            describe_relation(
                summary.target_differs_from_base,
                summary.target_changes.len(),
                "change"
            )
        );
        print_changes("target changes since base", &summary.target_changes);
    }

    if summary.deletion_candidates.is_empty() {
        println!("deletions: none");
    } else {
        println!(
            "deletions: {}",
            summary
                .deletion_candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    if !summary.notes.is_empty() {
        println!();
        println!("notes:");
        for note in &summary.notes {
            println!("  - {note}");
        }
    }

    if !summary.next_actions.is_empty() {
        println!();
        println!("next:");
        for action in &summary.next_actions {
            println!("  - {action}");
        }
    }
}

pub(crate) fn managed_paths(
    client: &JjClient,
    target: &RevisionSummary,
) -> Result<BTreeSet<PathBuf>> {
    let current = client.current_revision()?;
    if client.is_ancestor(target, &current)? {
        return client
            .list_files(&current)
            .map(|files| files.into_iter().collect());
    }

    let mut paths = BTreeSet::new();
    paths.extend(client.list_files(target)?);
    paths.extend(client.list_files(&current)?);
    Ok(paths)
}

fn classify_change(base: Option<&ManagedEntry>, other: Option<&ManagedEntry>) -> FileChangeKind {
    match (base, other) {
        (None, None) => FileChangeKind::Unchanged,
        (Some(_), None) => FileChangeKind::Deleted,
        (None, Some(_)) => FileChangeKind::Added,
        (Some(left), Some(right)) if left == right => FileChangeKind::Unchanged,
        (Some(ManagedEntry::Conflict), _) | (_, Some(ManagedEntry::Conflict)) => {
            FileChangeKind::Conflict
        }
        (Some(ManagedEntry::Unsupported { .. }), _)
        | (_, Some(ManagedEntry::Unsupported { .. })) => FileChangeKind::Conflict,
        _ => FileChangeKind::Modified,
    }
}

fn describe_relation(differs: bool, count: usize, label: &str) -> String {
    if differs {
        format!(
            "differs from base ({count} {label}{})",
            if count == 1 { "" } else { "s" }
        )
    } else {
        "matches base".to_string()
    }
}

fn print_changes(title: &str, changes: &[FileStatusSummary]) {
    if changes.is_empty() {
        return;
    }

    println!("{title}:");
    for change in changes.iter().take(8) {
        println!(
            "  - {:<8} {}",
            kind_label(&change.kind),
            change.path.display()
        );
    }
    if changes.len() > 8 {
        println!("  - … {} more", changes.len() - 8);
    }
}

fn kind_label(kind: &FileChangeKind) -> &'static str {
    match kind {
        FileChangeKind::Added => "added",
        FileChangeKind::Modified => "modified",
        FileChangeKind::Deleted => "deleted",
        FileChangeKind::Conflict => "conflict",
        FileChangeKind::Unchanged => "same",
        FileChangeKind::Unknown => "unknown",
    }
}
