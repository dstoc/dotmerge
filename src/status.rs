use crate::cli::StatusArgs;
use crate::fs;
use crate::jj::{JjClient, JjSession};
use crate::model::{
    FileChangeKind, FileStatusSummary, ManagedEntry, ResumeState, Revision, SyncState,
    SyncStatusSummary,
};
use crate::util;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(crate) trait StatusSource {
    fn root_revision(&self) -> Result<Revision>;
    fn current_revision(&self) -> Result<Revision>;
    fn list_files(&self, rev: &Revision) -> Result<Vec<PathBuf>>;
    fn read_entries_at_rev(
        &self,
        rev: &Revision,
        paths: &BTreeSet<PathBuf>,
    ) -> Result<std::collections::BTreeMap<PathBuf, Option<ManagedEntry>>>;
    fn has_conflicts(&self, rev: &Revision) -> Result<bool>;
    fn bookmark_summary(&self, name: &str) -> Result<crate::model::BookmarkSummary>;
    fn is_ancestor(&self, ancestor: &Revision, descendant: &Revision) -> Result<bool>;
    fn resume_state(
        &self,
        base: &Revision,
        current_import: Option<&Revision>,
    ) -> Result<ResumeState>;
    fn working_copy_clean_hint(&self) -> Result<Option<bool>>;
}

pub fn run(args: StatusArgs) -> Result<()> {
    let home = util::home_dir()?;
    let client = JjClient::open(&args.common.repo)?;
    let repo_path = client.repo_path().to_path_buf();
    let session = client.begin()?;
    let target = session.resolve_rev(&args.common.target)?;
    let summary = collect(&session, &repo_path, &home, target)?;
    print_summary(&summary);
    Ok(())
}

pub(crate) fn collect(
    source: &impl StatusSource,
    repo_path: &Path,
    home: &Path,
    target: Revision,
) -> Result<SyncStatusSummary> {
    collect_with_repo_clean(source, repo_path, home, target, None)
}

pub(crate) fn collect_for_sync(
    source: &impl StatusSource,
    repo_path: &Path,
    home: &Path,
    target: Revision,
) -> Result<SyncStatusSummary> {
    collect_with_repo_clean(source, repo_path, home, target, Some(true))
}

fn collect_with_repo_clean(
    source: &impl StatusSource,
    repo_path: &Path,
    home: &Path,
    target: Revision,
    repo_clean_override: Option<bool>,
) -> Result<SyncStatusSummary> {
    let last_sync = source.bookmark_summary("last-sync")?;
    let current_import = source.bookmark_summary("current-import")?;
    let base = match &last_sync.revision {
        Some(revision) => revision.clone(),
        None => source.root_revision()?,
    };
    let current = source.current_revision()?;
    let resume_state = source.resume_state(&base, current_import.revision.as_ref())?;
    let mut summary = SyncStatusSummary::new(
        base.to_summary(),
        current_import.clone(),
        target.to_summary(),
        repo_path.to_path_buf(),
    );
    summary.last_sync_present = last_sync.exists;
    let target_already_applied = source.is_ancestor(&target, &current)?;
    summary.target_already_applied = target_already_applied;
    let managed_paths = managed_paths(source, &target)?;
    let base_entries = source.read_entries_at_rev(&base, &managed_paths)?;
    let target_entries = source.read_entries_at_rev(&target, &managed_paths)?;

    summary.repo_clean = Some(match repo_clean_override {
        Some(repo_clean) => repo_clean,
        None => is_working_copy_clean(source, repo_path)?,
    });

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
        if matches!(resume_state, ResumeState::Resumable) && !current.same(import_revision) {
            summary.prepared = Some(current.to_summary());
        }
    }

    summary.has_conflicts = source.has_conflicts(&current)?;
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

    // Derive the single SyncState (priority order: first match wins).
    summary.state = if summary.repo_clean == Some(false) {
        SyncState::RepoDirty
    } else if matches!(resume_state, ResumeState::Blocked { .. }) {
        SyncState::Blocked
    } else if summary.has_conflicts {
        SyncState::Conflict
    } else if summary.current_import.revision.is_some()
        && matches!(resume_state, ResumeState::Resumable)
    {
        SyncState::MergePrepared
    } else if summary.home_differs_from_base
        && summary.target_differs_from_base
        && !summary.target_already_applied
    {
        SyncState::Diverged
    } else if summary.target_differs_from_base && !summary.target_already_applied {
        SyncState::Incoming
    } else if summary.home_differs_from_base {
        SyncState::LocalChanges
    } else {
        SyncState::UpToDate
    };

    // Lift resume_state onto the summary for use by the renderer.
    summary.resume_state = resume_state;

    Ok(summary)
}

pub fn print_summary(summary: &SyncStatusSummary) {
    // --- State headline ---
    let (state_name, state_desc) = state_headline(&summary.state, summary);
    println!("state:   {state_name} — {state_desc}");
    println!();

    // --- Facts block ---
    let base_id = if summary.last_sync_present {
        summary.base.short_id()
    } else {
        "none".to_string()
    };
    println!("base:    last-sync     {base_id}");

    // target line
    let target_suffix = if summary.target_already_applied {
        "  (already applied)".to_string()
    } else if summary.target_differs_from_base {
        format!("  ({} ahead)", plural_changes(summary.target_changes.len()))
    } else {
        String::new()
    };
    println!(
        "target:  {}   {}{}",
        summary.target.expression,
        summary.target.short_id(),
        target_suffix
    );

    // import line
    match &summary.current_import.revision {
        None => println!("import:  none"),
        Some(revision) => {
            let at_suffix = if matches!(summary.state, SyncState::MergePrepared) {
                "  (prepared at @)"
            } else {
                ""
            };
            println!(
                "import:  current-import {}{}",
                revision.short_id(),
                at_suffix
            );
        }
    }

    // @ line — only for MergePrepared
    if matches!(summary.state, SyncState::MergePrepared) {
        if let Some(prepared) = &summary.prepared {
            println!(
                "@:       {}   (merge of import + target)",
                prepared.short_id()
            );
        }
    }

    // repo line
    let repo_clean_label = match summary.repo_clean {
        Some(true) => "  (clean)",
        Some(false) => "  (dirty)",
        None => "",
    };
    println!(
        "repo:    {}{}",
        summary.repo_path.display(),
        repo_clean_label
    );

    // --- Change lists ---
    if !summary.target_changes.is_empty() && !summary.target_already_applied {
        println!();
        print_changes(
            "incoming changes (target since base)",
            &summary.target_changes,
        );
    }
    if !summary.home_changes.is_empty() {
        println!();
        print_changes("local changes ($HOME since base)", &summary.home_changes);
    }

    // --- Notes ---
    if !summary.notes.is_empty() {
        println!();
        println!("notes:");
        for note in &summary.notes {
            println!("  - {note}");
        }
    }

    // --- sync will: line ---
    println!();
    let sync_will = sync_will_line(&summary.state, &summary.resume_state);
    println!("sync will:  {sync_will}");
}

fn state_headline(state: &SyncState, summary: &SyncStatusSummary) -> (String, String) {
    match state {
        SyncState::UpToDate => (
            "up to date".to_string(),
            "$HOME, repo, and target already agree".to_string(),
        ),
        SyncState::LocalChanges => (
            "local changes".to_string(),
            format!(
                "$HOME is {} ahead of last-sync",
                plural_changes(summary.home_changes.len())
            ),
        ),
        SyncState::Incoming => (
            "incoming".to_string(),
            format!(
                "target is {} ahead of last-sync",
                plural_changes(summary.target_changes.len())
            ),
        ),
        SyncState::Diverged => (
            "diverged".to_string(),
            format!(
                "$HOME is {} ahead and target is {} ahead of last-sync",
                plural_changes(summary.home_changes.len()),
                plural_changes(summary.target_changes.len())
            ),
        ),
        SyncState::MergePrepared => (
            "merge prepared".to_string(),
            "not yet exported".to_string(),
        ),
        SyncState::Conflict => (
            "conflict".to_string(),
            "jj conflicts at @ must be resolved before export".to_string(),
        ),
        SyncState::Blocked => (
            "blocked".to_string(),
            "current-import is not in a valid resume position".to_string(),
        ),
        SyncState::RepoDirty => (
            "repo dirty".to_string(),
            "repo working copy has uncommitted changes".to_string(),
        ),
    }
}

fn sync_will_line(state: &SyncState, resume_state: &ResumeState) -> String {
    match state {
        SyncState::UpToDate => {
            "nothing — $HOME, repo, and target already agree".to_string()
        }
        SyncState::LocalChanges => {
            "import the local $HOME changes and advance last-sync".to_string()
        }
        SyncState::Incoming => {
            "merge target into the imported $HOME state, then export".to_string()
        }
        SyncState::Diverged => {
            "import the local changes, merge target, then export".to_string()
        }
        SyncState::MergePrepared => {
            "export the prepared merge to $HOME and advance last-sync".to_string()
        }
        SyncState::Conflict => {
            "resolve the jj conflicts at `@` in the repo, then rerun dotmerge sync".to_string()
        }
        SyncState::Blocked => {
            if let ResumeState::Blocked { reason } = resume_state {
                format!(
                    "repair `current-import` until it satisfies the resume preconditions, then rerun `dotmerge sync` (reason: {reason})"
                )
            } else {
                "repair `current-import` until it satisfies the resume preconditions, then rerun `dotmerge sync`".to_string()
            }
        }
        SyncState::RepoDirty => {
            "clean the repo working copy, then rerun dotmerge sync".to_string()
        }
    }
}

pub(crate) fn managed_paths(
    source: &impl StatusSource,
    target: &Revision,
) -> Result<BTreeSet<PathBuf>> {
    let current = source.current_revision()?;
    if source.is_ancestor(target, &current)? {
        return source
            .list_files(&current)
            .map(|files| files.into_iter().collect());
    }

    let mut paths = BTreeSet::new();
    paths.extend(source.list_files(target)?);
    paths.extend(source.list_files(&current)?);
    Ok(paths)
}

pub(crate) fn is_working_copy_clean(source: &impl StatusSource, _repo_path: &Path) -> Result<bool> {
    source.working_copy_clean_hint()?.ok_or_else(|| {
        anyhow::anyhow!("StatusSource did not provide a working_copy_clean_hint")
    })
}

impl StatusSource for JjSession {
    fn root_revision(&self) -> Result<Revision> {
        JjSession::root_revision(self)
    }

    fn current_revision(&self) -> Result<Revision> {
        JjSession::current_revision(self)
    }

    fn list_files(&self, rev: &Revision) -> Result<Vec<PathBuf>> {
        JjSession::list_files(self, rev)
    }

    fn read_entries_at_rev(
        &self,
        rev: &Revision,
        paths: &BTreeSet<PathBuf>,
    ) -> Result<std::collections::BTreeMap<PathBuf, Option<ManagedEntry>>> {
        JjSession::read_entries_at_rev(self, rev, paths)
    }

    fn has_conflicts(&self, rev: &Revision) -> Result<bool> {
        JjSession::has_conflicts(self, rev)
    }

    fn bookmark_summary(&self, name: &str) -> Result<crate::model::BookmarkSummary> {
        JjSession::bookmark_summary(self, name)
    }

    fn is_ancestor(
        &self,
        ancestor: &Revision,
        descendant: &Revision,
    ) -> Result<bool> {
        JjSession::is_ancestor(self, ancestor, descendant)
    }

    fn resume_state(
        &self,
        base: &Revision,
        current_import: Option<&Revision>,
    ) -> Result<ResumeState> {
        JjSession::resume_state(self, base, current_import)
    }

    fn working_copy_clean_hint(&self) -> Result<Option<bool>> {
        Ok(Some(JjSession::is_working_copy_clean(self)?))
    }
}

pub(crate) fn classify_change(base: Option<&ManagedEntry>, other: Option<&ManagedEntry>) -> FileChangeKind {
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

fn plural_changes(count: usize) -> String {
    if count == 1 {
        "1 change".to_string()
    } else {
        format!("{count} changes")
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

pub(crate) fn kind_label(kind: &FileChangeKind) -> &'static str {
    match kind {
        FileChangeKind::Added => "added",
        FileChangeKind::Modified => "modified",
        FileChangeKind::Deleted => "deleted",
        FileChangeKind::Conflict => "conflict",
        FileChangeKind::Unchanged => "same",
    }
}
