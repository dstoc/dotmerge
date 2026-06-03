use crate::cli::AddArgs;
use crate::fs;
use crate::jj::JjClient;
use crate::model::ValidatedAddSource;
use crate::util;
use anyhow::{Result, anyhow};
use std::collections::HashSet;
use std::path::PathBuf;

struct PlannedAdd {
    source: ValidatedAddSource,
    destination_path: PathBuf,
}

pub fn run(args: AddArgs) -> Result<()> {
    let home = util::home_dir()?;
    let client = JjClient::open(&args.repo)?;

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
