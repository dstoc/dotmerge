use crate::model::{BookmarkSummary, RevisionSummary};
use crate::util::ensure_success;
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, Clone)]
pub struct JjClient {
    repo_path: PathBuf,
}

impl JjClient {
    pub fn open(repo_path: impl Into<PathBuf>) -> Result<Self> {
        let client = Self {
            repo_path: repo_path.into(),
        };
        client.verify_repo()?;
        Ok(client)
    }

    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    pub fn resolve_rev(&self, revset: &str) -> Result<RevisionSummary> {
        let output = self
            .jj([
                "log",
                "-r",
                revset,
                "-T",
                "commit_id",
                "--no-graph",
                "--limit",
                "2",
            ])?
            .output()?;
        let output = ensure_success("jj", &["log", "-r", revset], output)?;
        let resolved = String::from_utf8(output.stdout)?;
        let mut ids = resolved
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());

        let first = ids
            .next()
            .ok_or_else(|| anyhow!("`{revset}` resolved to no revisions"))?;
        if ids.next().is_some() {
            return Err(anyhow!("`{revset}` resolved to more than one revision"));
        }

        Ok(RevisionSummary::resolved(revset, first))
    }

    pub fn read_tree(&self, _rev: &RevisionSummary) -> Result<String> {
        Err(anyhow!("read_tree not implemented yet"))
    }

    pub fn list_files(&self, rev: &RevisionSummary) -> Result<Vec<PathBuf>> {
        let rev_arg = self.rev_arg(rev);
        let args = vec![
            "file".to_string(),
            "list".to_string(),
            "--quiet".to_string(),
            "-r".to_string(),
            rev_arg.to_string(),
        ];
        let output = self
            .run_jj(args)
            .with_context(|| format!("failed to list files for revision `{rev_arg}`"))?;
        let stdout =
            String::from_utf8(output.stdout).context("jj file list returned invalid UTF-8")?;
        Ok(stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect())
    }

    pub fn file_at_rev(&self, rev: &RevisionSummary, path: &Path) -> Result<Option<Vec<u8>>> {
        let rev_arg = self.rev_arg(rev);
        let path_arg = path.to_string_lossy().into_owned();
        let args = vec![
            "file".to_string(),
            "show".to_string(),
            "--quiet".to_string(),
            "-r".to_string(),
            rev_arg.to_string(),
            path_arg.clone(),
        ];
        let output = self
            .jj(args.iter().map(String::as_str))?
            .output()
            .with_context(|| format!("failed to read `{path_arg}` at revision `{rev_arg}`"))?;
        if output.status.success() {
            return Ok(Some(output.stdout));
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("No such path:") {
            return Ok(None);
        }

        ensure_success("jj", &args, output)
            .with_context(|| format!("failed to read `{path_arg}` at revision `{rev_arg}`"))?;
        unreachable!("ensure_success returns on success or error")
    }

    pub fn set_bookmark(&self, name: &str, rev: &RevisionSummary) -> Result<()> {
        let resolved = rev
            .resolved
            .as_deref()
            .ok_or_else(|| anyhow!("bookmark `{name}` requires a resolved revision"))?;
        let output = self
            .jj(["bookmark", "set", name, "-r", resolved])?
            .output()?;
        ensure_success("jj", &["bookmark", "set", name, "-r", resolved], output)?;
        Ok(())
    }

    pub fn clear_bookmark(&self, name: &str) -> Result<()> {
        let output = self.jj(["bookmark", "delete", name])?.output()?;
        ensure_success("jj", &["bookmark", "delete", name], output)?;
        Ok(())
    }

    pub fn create_or_refresh_import(
        &self,
        _base: &RevisionSummary,
        _home_state: &Path,
    ) -> Result<RevisionSummary> {
        Err(anyhow!("create_or_refresh_import not implemented yet"))
    }

    pub fn merge_revisions(
        &self,
        _left: &RevisionSummary,
        _right: &RevisionSummary,
    ) -> Result<RevisionSummary> {
        Err(anyhow!("merge_revisions not implemented yet"))
    }

    pub fn has_conflicts(&self, rev: &RevisionSummary) -> Result<bool> {
        let rev_arg = self.rev_arg(rev);
        let args = vec![
            "log".to_string(),
            "-r".to_string(),
            rev_arg.to_string(),
            "-T".to_string(),
            "conflict".to_string(),
            "--no-graph".to_string(),
            "--quiet".to_string(),
            "--color=never".to_string(),
        ];
        let output = self
            .run_jj(args)
            .with_context(|| format!("failed to check conflicts for revision `{rev_arg}`"))?;
        let stdout = String::from_utf8(output.stdout).context("jj log returned invalid UTF-8")?;
        let value = stdout.trim();
        match value {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(anyhow!(
                "unexpected conflict status `{value}` for revision `{rev_arg}`"
            )),
        }
    }

    pub fn bookmark_summary(&self, name: &str) -> Result<BookmarkSummary> {
        let args = vec![
            "bookmark".to_string(),
            "list".to_string(),
            name.to_string(),
            "--quiet".to_string(),
            "--color=never".to_string(),
        ];
        let output = self
            .run_jj(args)
            .with_context(|| format!("failed to inspect bookmark `{name}`"))?;
        let stdout =
            String::from_utf8(output.stdout).context("jj bookmark list returned invalid UTF-8")?;
        let mut lines = stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty());
        let first = match lines.next() {
            Some(line) => line,
            None => return Ok(BookmarkSummary::missing(name)),
        };
        if lines.next().is_some() {
            return Err(anyhow!(
                "bookmark pattern `{name}` matched more than one bookmark"
            ));
        }

        let listed_name = first
            .split_once(':')
            .map(|(bookmark_name, _)| bookmark_name.trim())
            .filter(|bookmark_name| !bookmark_name.is_empty())
            .ok_or_else(|| anyhow!("failed to parse bookmark listing for `{name}`"))?;
        let revision = self
            .resolve_rev(listed_name)
            .with_context(|| format!("failed to resolve bookmark `{listed_name}`"))?;

        Ok(BookmarkSummary {
            name: listed_name.to_string(),
            revision: Some(revision),
            exists: true,
        })
    }

    pub fn is_working_copy_clean(&self) -> Result<bool> {
        let args = vec![
            "status".to_string(),
            "--quiet".to_string(),
            "--color=never".to_string(),
        ];
        let output = self
            .run_jj(args)
            .context("failed to inspect working copy status")?;
        let stdout =
            String::from_utf8(output.stdout).context("jj status returned invalid UTF-8")?;
        Ok(stdout.contains("The working copy has no changes."))
    }

    pub fn create_new_change(
        &self,
        parents: &[RevisionSummary],
        message: &str,
    ) -> Result<RevisionSummary> {
        if parents.is_empty() {
            return Err(anyhow!(
                "create_new_change requires at least one parent revision"
            ));
        }

        let mut args = vec!["new".to_string(), "--no-edit".to_string()];
        for parent in parents {
            args.push(self.rev_arg(parent).to_string());
        }

        let output = self
            .run_jj(args.clone())
            .context("failed to create a new jj change")?;
        let new_rev = self
            .parse_created_commit(&output)
            .context("failed to determine the new jj revision from `jj new` output")?;
        self.describe_revision(&new_rev, message)
            .with_context(|| format!("failed to describe new revision `{new_rev}`"))?;
        self.resolve_rev(&new_rev)
            .with_context(|| format!("failed to resolve new revision `{new_rev}`"))
    }

    pub fn create_merge_change(
        &self,
        left: &RevisionSummary,
        right: &RevisionSummary,
        message: &str,
    ) -> Result<RevisionSummary> {
        self.create_new_change(&[left.clone(), right.clone()], message)
    }

    pub fn describe_current(&self, message: &str) -> Result<()> {
        self.describe_revision("@", message)
    }

    fn verify_repo(&self) -> Result<()> {
        let output = self.jj(["workspace", "list"])?.output()?;
        ensure_success("jj", &["workspace", "list"], output)?;
        Ok(())
    }

    fn describe_revision(&self, revset: &str, message: &str) -> Result<()> {
        let args = vec![
            "describe".to_string(),
            revset.to_string(),
            "-m".to_string(),
            message.to_string(),
        ];
        self.run_jj(args)
            .with_context(|| format!("failed to describe revision `{revset}`"))?;
        Ok(())
    }

    fn run_jj(&self, args: Vec<String>) -> Result<Output> {
        let output = self
            .jj(args.iter().map(String::as_str))?
            .output()
            .with_context(|| format!("failed to run `jj {}`", args.join(" ")))?;
        ensure_success("jj", &args, output)
    }

    fn rev_arg<'a>(&self, rev: &'a RevisionSummary) -> &'a str {
        rev.resolved.as_deref().unwrap_or(&rev.expression)
    }

    fn parse_created_commit(&self, output: &Output) -> Result<String> {
        let stderr =
            String::from_utf8(output.stderr.clone()).context("jj new returned invalid UTF-8")?;
        for line in stderr.lines().map(str::trim) {
            if !line.starts_with("Created new commit ") {
                continue;
            }

            let commit_id = line
                .split_whitespace()
                .nth(4)
                .ok_or_else(|| anyhow!("missing commit id in `jj new` output: {line}"))?;
            return Ok(commit_id.to_string());
        }

        Err(anyhow!("`jj new` did not report the created commit"))
    }

    fn jj<I, S>(&self, args: I) -> Result<Command>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::new("jj");
        command.current_dir(&self.repo_path);
        command.args(args);
        Ok(command)
    }
}
