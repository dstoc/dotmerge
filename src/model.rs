use std::path::PathBuf;
use jj_lib::backend::CommitId;
use jj_lib::object_id::ObjectId as _;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revision {
    id: CommitId,
    label: String,
}

impl Revision {
    pub(crate) fn new(id: CommitId, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
        }
    }

    pub(crate) fn id(&self) -> &CommitId {
        &self.id
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn short_id(&self) -> String {
        self.id.hex()[..8].to_string()
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        self.id == other.id
    }

    pub(crate) fn to_summary(&self) -> RevisionSummary {
        RevisionSummary::resolved(self.label.clone(), self.id.hex())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RevisionSummary {
    pub(crate) expression: String,
    pub(crate) resolved: Option<String>,
}

impl RevisionSummary {
    pub(crate) fn resolved(expression: impl Into<String>, resolved: impl Into<String>) -> Self {
        Self {
            expression: expression.into(),
            resolved: Some(resolved.into()),
        }
    }

    pub(crate) fn short_id(&self) -> String {
        self.resolved
            .as_deref()
            .map(|hex| hex.chars().take(8).collect())
            .unwrap_or_else(|| self.expression.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BookmarkSummary {
    pub(crate) name: String,
    pub(crate) revision: Option<Revision>,
    pub(crate) exists: bool,
}

impl BookmarkSummary {
    pub(crate) fn missing(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            revision: None,
            exists: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResumeState {
    Fresh,
    Resumable,
    Blocked { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SyncState {
    UpToDate,
    LocalChanges,
    Incoming,
    Diverged,
    MergePrepared,
    Conflict,
    Blocked,
    RepoDirty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ManagedEntry {
    File { contents: Vec<u8>, executable: bool },
    Symlink { target: PathBuf },
    Conflict,
    Unsupported { kind: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FileChangeKind {
    Added,
    Modified,
    Deleted,
    Conflict,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileStatusSummary {
    pub(crate) path: PathBuf,
    pub(crate) kind: FileChangeKind,
    pub(crate) detail: Option<String>,
}

impl FileStatusSummary {
    pub(crate) fn new(path: impl Into<PathBuf>, kind: FileChangeKind) -> Self {
        Self {
            path: path.into(),
            kind,
            detail: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyncStatusSummary {
    pub(crate) base: RevisionSummary,
    pub(crate) current_import: BookmarkSummary,
    pub(crate) target: RevisionSummary,
    pub(crate) repo_path: PathBuf,
    pub(crate) prepared: Option<RevisionSummary>,
    pub(crate) repo_clean: Option<bool>,
    pub(crate) home_differs_from_base: bool,
    pub(crate) target_differs_from_base: bool,
    pub(crate) target_already_applied: bool,
    pub(crate) has_conflicts: bool,
    pub(crate) home_changes: Vec<FileStatusSummary>,
    pub(crate) target_changes: Vec<FileStatusSummary>,
    pub(crate) deletion_candidates: Vec<PathBuf>,
    pub(crate) notes: Vec<String>,
    pub(crate) last_sync_present: bool,
    pub(crate) resume_state: ResumeState,
    pub(crate) state: SyncState,
}

impl SyncStatusSummary {
    pub(crate) fn new(
        base: RevisionSummary,
        current_import: BookmarkSummary,
        target: RevisionSummary,
        repo_path: PathBuf,
    ) -> Self {
        Self {
            base,
            current_import,
            target,
            repo_path,
            prepared: None,
            repo_clean: None,
            home_differs_from_base: false,
            target_differs_from_base: false,
            target_already_applied: false,
            has_conflicts: false,
            home_changes: Vec::new(),
            target_changes: Vec::new(),
            deletion_candidates: Vec::new(),
            notes: Vec::new(),
            last_sync_present: false,
            resume_state: ResumeState::Fresh,
            state: SyncState::UpToDate,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImportOutcome {
    pub(crate) revision: Revision,
    pub(crate) imported: Vec<FileStatusSummary>, // base -> $HOME delta; empty when $HOME has no drift
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MergeOutcome {
    NoOp { revision: Revision },        // target already contained the import (right ⊆ left)
    FastForward { revision: Revision }, // advanced to target (left ⊆ right)
    Merged { revision: Revision },      // a real merge commit was created
}

impl MergeOutcome {
    pub(crate) fn revision(&self) -> &Revision {
        match self {
            MergeOutcome::NoOp { revision }
            | MergeOutcome::FastForward { revision }
            | MergeOutcome::Merged { revision } => revision,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AddSourceKind {
    File { mode: u32 },
    Symlink { target: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValidatedAddSource {
    pub(crate) input_path: PathBuf,
    pub(crate) source_path: PathBuf,
    pub(crate) repo_path: PathBuf,
    pub(crate) kind: AddSourceKind,
}
