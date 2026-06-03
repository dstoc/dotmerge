use crate::model::{BookmarkSummary, RevisionSummary};
use anyhow::{Context, Result, anyhow};
use chrono::Local;
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::config::StackedConfig;
use jj_lib::conflicts::{
    ConflictMarkerStyle, ConflictMaterializeOptions, MaterializedTreeValue,
    materialize_merge_result_to_bytes, materialize_tree_value,
};
use jj_lib::files::FileMergeHunkLevel;
use jj_lib::fileset::FilesetAliasesMap;
use jj_lib::merge::SameChange;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::RefTarget;
use jj_lib::repo::{ReadonlyRepo, Repo as _, StoreFactories};
use jj_lib::repo_path::{RepoPathBuf, RepoPathUiConverter};
use jj_lib::revset::{
    RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetParseContext,
    RevsetWorkspaceContext, SymbolResolver, UserRevsetExpression, parse,
};
use jj_lib::rewrite::merge_commit_trees;
use jj_lib::settings::UserSettings;
use jj_lib::time_util::DatePatternContext;
use jj_lib::tree_merge::MergeOptions;
use jj_lib::workspace::{Workspace, default_working_copy_factories};
use pollster::FutureExt as _;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct JjClient {
    workspace_root: PathBuf,
    settings: UserSettings,
}

impl JjClient {
    pub fn open(repo_path: impl Into<PathBuf>) -> Result<Self> {
        let workspace_root = find_workspace_root(repo_path.into())?;
        let settings = UserSettings::from_config(StackedConfig::with_defaults())
            .context("failed to construct default jj settings")?;
        let client = Self {
            workspace_root,
            settings,
        };
        client.load_workspace_and_repo()?;
        Ok(client)
    }

    pub fn repo_path(&self) -> &Path {
        &self.workspace_root
    }

    pub fn resolve_rev(&self, revset: &str) -> Result<RevisionSummary> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_commit_by_revset(&workspace, &repo, revset)?;
        Ok(self.revision_summary(revset, commit.id()))
    }

    pub fn read_tree(&self, rev: &RevisionSummary) -> Result<String> {
        let files = self.list_files(rev)?;
        Ok(files
            .into_iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub fn list_files(&self, rev: &RevisionSummary) -> Result<Vec<PathBuf>> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_summary_to_commit(&workspace, &repo, rev)?;
        let mut files = commit
            .tree()
            .entries()
            .map(|(path, _)| path.to_fs_path(Path::new("")))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to convert repo paths to filesystem paths")?;
        files.sort();
        Ok(files)
    }

    pub fn file_at_rev(&self, rev: &RevisionSummary, path: &Path) -> Result<Option<Vec<u8>>> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_summary_to_commit(&workspace, &repo, rev)?;
        let repo_path = self.parse_repo_path(path)?;
        let tree = commit.tree();
        let value = tree
            .path_value(repo_path.as_ref())
            .block_on()
            .with_context(|| {
                format!(
                    "failed to read `{}` from revision `{}`",
                    path.display(),
                    self.rev_label(rev)
                )
            })?;
        let materialized =
            materialize_tree_value(repo.store(), repo_path.as_ref(), value, tree.labels())
                .block_on()
                .with_context(|| {
                    format!(
                        "failed to materialize `{}` from revision `{}`",
                        path.display(),
                        self.rev_label(rev)
                    )
                })?;

        match materialized {
            MaterializedTreeValue::Absent => Ok(None),
            MaterializedTreeValue::File(mut file) => file
                .read_all(repo_path.as_ref())
                .block_on()
                .map(Some)
                .with_context(|| {
                    format!(
                        "failed to read `{}` from revision `{}`",
                        path.display(),
                        self.rev_label(rev)
                    )
                }),
            MaterializedTreeValue::FileConflict(file) => {
                let bytes = materialize_merge_result_to_bytes(
                    &file.contents,
                    &file.labels,
                    &ConflictMaterializeOptions {
                        marker_style: ConflictMarkerStyle::Diff,
                        marker_len: None,
                        merge: MergeOptions {
                            hunk_level: FileMergeHunkLevel::Line,
                            same_change: SameChange::Accept,
                        },
                    },
                );
                Ok(Some(bytes.to_vec()))
            }
            MaterializedTreeValue::Symlink { target, .. } => Ok(Some(target.into_bytes())),
            MaterializedTreeValue::Tree(_) => Err(anyhow!(
                "`{}` is a directory in `{}`",
                path.display(),
                self.rev_label(rev)
            )),
            MaterializedTreeValue::GitSubmodule(_) => Err(anyhow!(
                "`{}` is a git submodule in `{}`",
                path.display(),
                self.rev_label(rev)
            )),
            MaterializedTreeValue::OtherConflict { .. } => Err(anyhow!(
                "`{}` has a non-file conflict in `{}`",
                path.display(),
                self.rev_label(rev)
            )),
            MaterializedTreeValue::AccessDenied(err) => Err(anyhow!(err))
                .with_context(|| format!("access denied reading `{}`", path.display())),
        }
    }

    pub fn set_bookmark(&self, name: &str, rev: &RevisionSummary) -> Result<()> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_summary_to_commit(&workspace, &repo, rev)?;
        let mut tx = repo.start_transaction();
        tx.repo_mut()
            .set_local_bookmark_target(name.as_ref(), RefTarget::normal(commit.id().clone()));
        tx.commit(format!("set bookmark {name}")).block_on()?;
        Ok(())
    }

    pub fn clear_bookmark(&self, name: &str) -> Result<()> {
        let (_, repo) = self.load_workspace_and_repo()?;
        let mut tx = repo.start_transaction();
        tx.repo_mut()
            .set_local_bookmark_target(name.as_ref(), RefTarget::absent());
        tx.commit(format!("clear bookmark {name}")).block_on()?;
        Ok(())
    }

    pub fn create_or_refresh_import(
        &self,
        _base: &RevisionSummary,
        _home_state: &Path,
    ) -> Result<RevisionSummary> {
        Err(anyhow!(
            "creating or refreshing an import from filesystem state is reserved for the next milestone"
        ))
    }

    pub fn merge_revisions(
        &self,
        left: &RevisionSummary,
        right: &RevisionSummary,
    ) -> Result<RevisionSummary> {
        self.create_merge_change(left, right, "dotmerge merge")
    }

    pub fn has_conflicts(&self, rev: &RevisionSummary) -> Result<bool> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_summary_to_commit(&workspace, &repo, rev)?;
        Ok(commit.has_conflict())
    }

    pub fn bookmark_summary(&self, name: &str) -> Result<BookmarkSummary> {
        let (_, repo) = self.load_workspace_and_repo()?;
        let target = repo.view().get_local_bookmark(name.as_ref());
        match target.as_resolved() {
            Some(Some(commit_id)) => Ok(BookmarkSummary {
                name: name.to_string(),
                revision: Some(self.revision_summary(name, commit_id)),
                exists: true,
            }),
            Some(None) => Ok(BookmarkSummary::missing(name)),
            None => Err(anyhow!("local bookmark `{name}` is conflicted")),
        }
    }

    pub fn is_working_copy_clean(&self) -> Result<bool> {
        Err(anyhow!(
            "working-copy cleanliness check is not implemented with jj-lib yet; refusing to assume the workspace is clean"
        ))
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

        let (workspace, repo) = self.load_workspace_and_repo()?;
        let parent_commits = parents
            .iter()
            .map(|parent| self.resolve_summary_to_commit(&workspace, &repo, parent))
            .collect::<Result<Vec<_>>>()?;
        let parent_ids = parent_commits
            .iter()
            .map(|commit| commit.id().clone())
            .collect::<Vec<_>>();
        let tree = merge_commit_trees(repo.as_ref(), &parent_commits)
            .block_on()
            .context("failed to derive initial tree for new change")?;

        let mut tx = repo.start_transaction();
        let commit = tx
            .repo_mut()
            .new_commit(parent_ids, tree)
            .set_description(message)
            .write()
            .block_on()
            .context("failed to write new commit")?;
        tx.commit(format!("create commit {}", commit.id().hex()))
            .block_on()?;
        Ok(self.revision_summary(commit.id().hex(), commit.id()))
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
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let current = self.resolve_commit_by_revset(&workspace, &repo, "@")?;
        let mut tx = repo.start_transaction();
        tx.repo_mut()
            .rewrite_commit(&current)
            .set_description(message)
            .write()
            .block_on()
            .context("failed to rewrite current commit")?;
        tx.repo_mut()
            .rebase_descendants()
            .block_on()
            .context("failed to rebase descendants after rewriting current commit")?;
        tx.commit("describe current commit").block_on()?;
        Ok(())
    }

    fn load_workspace_and_repo(&self) -> Result<(Workspace, Arc<ReadonlyRepo>)> {
        let workspace = Workspace::load(
            &self.settings,
            &self.workspace_root,
            &StoreFactories::default(),
            &default_working_copy_factories(),
        )
        .with_context(|| {
            format!(
                "failed to load jj workspace at `{}`",
                self.workspace_root.display()
            )
        })?;
        let repo = workspace
            .repo_loader()
            .load_at_head()
            .block_on()
            .context("failed to load repo state at operation head")?;
        Ok((workspace, repo))
    }

    fn resolve_summary_to_commit(
        &self,
        workspace: &Workspace,
        repo: &Arc<ReadonlyRepo>,
        rev: &RevisionSummary,
    ) -> Result<Commit> {
        if let Some(hex) = &rev.resolved {
            let commit_id = CommitId::try_from_hex(hex)
                .ok_or_else(|| anyhow!("invalid commit id `{hex}` stored in revision summary"))?;
            return repo
                .store()
                .get_commit(&commit_id)
                .with_context(|| format!("failed to load commit `{hex}`"));
        }
        self.resolve_commit_by_revset(workspace, repo, &rev.expression)
    }

    fn resolve_commit_by_revset(
        &self,
        workspace: &Workspace,
        repo: &Arc<ReadonlyRepo>,
        revset: &str,
    ) -> Result<Commit> {
        let extensions = RevsetExtensions::default();
        let expression = self.parse_revset(revset, workspace, &extensions)?;
        let symbol_resolver = SymbolResolver::new(repo.as_ref(), extensions.symbol_resolvers());
        let resolved = expression
            .resolve_user_expression(repo.as_ref(), &symbol_resolver)
            .with_context(|| format!("failed to resolve revset `{revset}`"))?;
        let evaluated = resolved
            .evaluate(repo.as_ref())
            .with_context(|| format!("failed to evaluate revset `{revset}`"))?;
        let mut commits = evaluated.commit_change_ids();
        let first = commits
            .next()
            .transpose()?
            .ok_or_else(|| anyhow!("`{revset}` resolved to no revisions"))?;
        if commits.next().transpose()?.is_some() {
            return Err(anyhow!("`{revset}` resolved to more than one revision"));
        }
        repo.store()
            .get_commit(&first.0)
            .with_context(|| format!("failed to load commit for revset `{revset}`"))
    }

    fn parse_revset(
        &self,
        revset: &str,
        workspace: &Workspace,
        extensions: &RevsetExtensions,
    ) -> Result<Arc<UserRevsetExpression>> {
        let mut diagnostics = RevsetDiagnostics::new();
        let aliases = RevsetAliasesMap::new();
        let fileset_aliases = FilesetAliasesMap::new();
        let ui = RepoPathUiConverter::Fs {
            cwd: self.workspace_root.clone(),
            base: self.workspace_root.clone(),
        };
        let context = RevsetParseContext {
            aliases_map: &aliases,
            local_variables: HashMap::new(),
            user_email: self.settings.user_email(),
            date_pattern_context: DatePatternContext::from(Local::now().fixed_offset()),
            default_ignored_remote: None,
            fileset_aliases_map: &fileset_aliases,
            use_glob_by_default: true,
            extensions,
            workspace: Some(RevsetWorkspaceContext {
                path_converter: &ui,
                workspace_name: workspace.workspace_name(),
            }),
        };
        parse(&mut diagnostics, revset, &context)
            .with_context(|| format!("failed to parse revset `{revset}`"))
    }

    fn parse_repo_path(&self, path: &Path) -> Result<RepoPathBuf> {
        RepoPathBuf::parse_fs_path(&self.workspace_root, &self.workspace_root, path).map_err(
            |err| {
                anyhow!(
                    "failed to interpret `{}` as a repo-relative path: {err}",
                    path.display()
                )
            },
        )
    }

    fn revision_summary(
        &self,
        expression: impl Into<String>,
        commit_id: &CommitId,
    ) -> RevisionSummary {
        RevisionSummary::resolved(expression, commit_id.hex())
    }

    fn rev_label<'a>(&self, rev: &'a RevisionSummary) -> &'a str {
        rev.resolved.as_deref().unwrap_or(&rev.expression)
    }
}

fn find_workspace_root(path: PathBuf) -> Result<PathBuf> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| anyhow!("`{}` has no parent directory", path.display()))?
    };

    for candidate in start.ancestors() {
        if candidate.join(".jj").is_dir() {
            return std::fs::canonicalize(candidate).with_context(|| {
                format!(
                    "failed to canonicalize workspace root `{}`",
                    candidate.display()
                )
            });
        }
    }

    Err(anyhow!(
        "could not find a jj workspace root from `{}`",
        start.display()
    ))
}
