use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionSummary {
    pub expression: String,
    pub resolved: Option<String>,
    pub exact: bool,
}

impl RevisionSummary {
    pub fn unresolved(expression: impl Into<String>) -> Self {
        Self {
            expression: expression.into(),
            resolved: None,
            exact: false,
        }
    }

    pub fn resolved(expression: impl Into<String>, resolved: impl Into<String>) -> Self {
        Self {
            expression: expression.into(),
            resolved: Some(resolved.into()),
            exact: true,
        }
    }

    pub fn short_id(&self) -> String {
        self.resolved
            .as_deref()
            .map(|hex| hex.chars().take(8).collect())
            .unwrap_or_else(|| self.expression.clone())
    }

    pub fn same_revision(&self, other: &Self) -> bool {
        match (&self.resolved, &other.resolved) {
            (Some(left), Some(right)) => left == right,
            _ => self.expression == other.expression,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkSummary {
    pub name: String,
    pub revision: Option<RevisionSummary>,
    pub exists: bool,
}

impl BookmarkSummary {
    pub fn missing(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            revision: None,
            exists: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManagedEntry {
    File { contents: Vec<u8>, executable: bool },
    Symlink { target: PathBuf },
    Conflict,
    Unsupported { kind: String },
}

impl ManagedEntry {
    pub fn describe(&self) -> &'static str {
        match self {
            ManagedEntry::File { .. } => "file",
            ManagedEntry::Symlink { .. } => "symlink",
            ManagedEntry::Conflict => "conflict",
            ManagedEntry::Unsupported { .. } => "unsupported",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
    Conflict,
    Unchanged,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatusSummary {
    pub path: PathBuf,
    pub kind: FileChangeKind,
    pub detail: Option<String>,
}

impl FileStatusSummary {
    pub fn new(path: impl Into<PathBuf>, kind: FileChangeKind) -> Self {
        Self {
            path: path.into(),
            kind,
            detail: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncStatusSummary {
    pub base: RevisionSummary,
    pub current_import: BookmarkSummary,
    pub target: RevisionSummary,
    pub repo_path: PathBuf,
    pub prepared: Option<RevisionSummary>,
    pub repo_clean: Option<bool>,
    pub home_differs_from_base: bool,
    pub target_differs_from_base: bool,
    pub target_already_applied: bool,
    pub has_conflicts: bool,
    pub home_changes: Vec<FileStatusSummary>,
    pub target_changes: Vec<FileStatusSummary>,
    pub deletion_candidates: Vec<PathBuf>,
    pub notes: Vec<String>,
    pub next_actions: Vec<String>,
}

impl SyncStatusSummary {
    pub fn new(
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
            next_actions: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddSourceKind {
    File { mode: u32 },
    Symlink { target: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedAddSource {
    pub input_path: PathBuf,
    pub source_path: PathBuf,
    pub repo_path: PathBuf,
    pub kind: AddSourceKind,
}
