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
    pub home_changes: Vec<FileStatusSummary>,
    pub target_changes: Vec<FileStatusSummary>,
    pub deletion_candidates: Vec<PathBuf>,
    pub notes: Vec<String>,
}

impl SyncStatusSummary {
    pub fn new(
        base: RevisionSummary,
        current_import: BookmarkSummary,
        target: RevisionSummary,
    ) -> Self {
        Self {
            base,
            current_import,
            target,
            home_changes: Vec::new(),
            target_changes: Vec::new(),
            deletion_candidates: Vec::new(),
            notes: Vec::new(),
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
