use crate::fs;
use crate::model::RevisionSummary;
use crate::status::StatusSource;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(crate) fn export_revision_to_home(
    session: &impl StatusSource,
    home: &Path,
    revision: &RevisionSummary,
    managed_paths: &BTreeSet<PathBuf>,
) -> Result<()> {
    let entries = session.read_entries_at_rev(revision, managed_paths)?;
    let export_entries = entries
        .into_iter()
        .filter_map(|(path, entry)| entry.map(|entry| (path, entry)))
        .collect::<Vec<_>>();
    fs::export_home_entries(home, &export_entries)?;
    Ok(())
}
