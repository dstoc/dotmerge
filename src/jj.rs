use crate::import;
use crate::model::{BookmarkSummary, ManagedEntry, ResumeState, RevisionSummary};
use anyhow::{anyhow, Context, Result};
use chrono::Local;
use jj_lib::backend::CommitId;
use jj_lib::commit::Commit;
use jj_lib::config::StackedConfig;
use jj_lib::conflicts::{
    materialize_merge_result_to_bytes, materialize_tree_value, ConflictMarkerStyle,
    ConflictMaterializeOptions, MaterializedTreeValue,
};
use jj_lib::files::FileMergeHunkLevel;
use jj_lib::fileset::FilesetAliasesMap;
use jj_lib::gitignore::GitIgnoreFile;
use jj_lib::matchers::{EverythingMatcher, NothingMatcher};
use jj_lib::merge::SameChange;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::RefTarget;
use jj_lib::repo::{MutableRepo, ReadonlyRepo, Repo as _, StoreFactories};
use jj_lib::repo_path::{RepoPath, RepoPathBuf, RepoPathUiConverter};
use jj_lib::revset::{
    parse, RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetParseContext,
    RevsetWorkspaceContext, SymbolResolver, UserRevsetExpression,
};
use jj_lib::rewrite::merge_commit_trees;
use jj_lib::settings::UserSettings;
use jj_lib::time_util::DatePatternContext;
use jj_lib::transaction::Transaction;
use jj_lib::tree_merge::MergeOptions;
use jj_lib::working_copy::SnapshotOptions;
use jj_lib::workspace::{default_working_copy_factories, Workspace};
use pollster::FutureExt as _;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct JjClient {
    workspace_root: PathBuf,
    settings: UserSettings,
}

pub(crate) struct JjSession {
    workspace: Workspace,
    tx: Transaction,
    settings: UserSettings,
    dirty: bool,
    pending_checkout: Option<Commit>,
}

impl JjClient {
    pub(crate) fn open(repo_path: impl Into<PathBuf>) -> Result<Self> {
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

    pub(crate) fn begin(&self) -> Result<JjSession> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let tx = repo.start_transaction();
        Ok(JjSession {
            workspace,
            tx,
            settings: self.settings.clone(),
            dirty: false,
            pending_checkout: None,
        })
    }

    pub(crate) fn repo_path(&self) -> &Path {
        &self.workspace_root
    }

    pub(crate) fn working_copy_path(&self, repo_relative: &Path) -> Result<PathBuf> {
        if repo_relative.as_os_str().is_empty() {
            return Err(anyhow!("repo-relative path must not be empty"));
        }
        if repo_relative.is_absolute() {
            return Err(anyhow!(
                "repo-relative path must not be absolute: `{}`",
                repo_relative.display()
            ));
        }

        self.parse_repo_path(repo_relative)?;
        Ok(self.workspace_root.join(repo_relative))
    }

    pub(crate) fn resolve_rev(&self, revset: &str) -> Result<RevisionSummary> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_commit_by_revset(&workspace, &repo, revset)?;
        Ok(self.revision_summary(revset, commit.id()))
    }

    pub(crate) fn root_revision(&self) -> Result<RevisionSummary> {
        let (_, repo) = self.load_workspace_and_repo()?;
        let root = repo.store().root_commit();
        Ok(self.revision_summary("empty-tree", root.id()))
    }

    pub(crate) fn current_revision(&self) -> Result<RevisionSummary> {
        self.resolve_rev("@")
    }

    pub(crate) fn list_files(&self, rev: &RevisionSummary) -> Result<Vec<PathBuf>> {
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

    pub(crate) fn read_entries_at_rev(
        &self,
        rev: &RevisionSummary,
        paths: &BTreeSet<PathBuf>,
    ) -> Result<BTreeMap<PathBuf, Option<ManagedEntry>>> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_summary_to_commit(&workspace, &repo, rev)?;
        let tree = commit.tree();
        let mut entries = BTreeMap::new();

        for path in paths {
            let repo_path = self.parse_repo_path(path)?;
            let entry =
                self.read_entry_from_tree(repo.as_ref(), &tree, repo_path.as_ref(), path)?;
            entries.insert(path.clone(), entry);
        }

        Ok(entries)
    }

    pub(crate) fn resume_state(
        &self,
        base: &RevisionSummary,
        current_import: Option<&RevisionSummary>,
    ) -> Result<ResumeState> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let current = self.current_revision()?;
        let current_commit = self.resolve_summary_to_commit(&workspace, &repo, &current)?;
        let current_is_disposable =
            import::is_disposable_sync_placeholder(repo.as_ref(), &current_commit)?;

        match current_import {
            None => Ok(ResumeState::Fresh),
            Some(current_import) => {
                let import_commit =
                    self.resolve_summary_to_commit(&workspace, &repo, current_import)?;
                let current_import_has_conflicts = self.has_conflicts(current_import)?;
                let current_import_is_descendant_of_base =
                    self.is_ancestor(base, current_import)?;
                Ok(resume_state_for_commits(
                    base,
                    &current,
                    &current_commit,
                    current_import,
                    &import_commit,
                    current_import_has_conflicts,
                    current_import_is_descendant_of_base,
                    current_is_disposable,
                ))
            }
        }
    }

    pub(crate) fn has_conflicts(&self, rev: &RevisionSummary) -> Result<bool> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_summary_to_commit(&workspace, &repo, rev)?;
        Ok(commit.has_conflict())
    }

    pub(crate) fn bookmark_summary(&self, name: &str) -> Result<BookmarkSummary> {
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

    pub(crate) fn is_ancestor(
        &self,
        ancestor: &RevisionSummary,
        descendant: &RevisionSummary,
    ) -> Result<bool> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let ancestor_commit = self.resolve_summary_to_commit(&workspace, &repo, ancestor)?;
        let descendant_commit = self.resolve_summary_to_commit(&workspace, &repo, descendant)?;
        repo.index()
            .is_ancestor(ancestor_commit.id(), descendant_commit.id())
            .context("failed to query jj ancestry")
    }

    pub(crate) fn is_working_copy_clean(&self) -> Result<bool> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let current = self.resolve_commit_by_revset(&workspace, &repo, "@")?;
        is_working_copy_clean_with_snapshot(&self.settings, &self.workspace_root, &current.tree())
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

    fn read_entry_from_tree(
        &self,
        repo: &ReadonlyRepo,
        tree: &jj_lib::merged_tree::MergedTree,
        repo_path: &RepoPath,
        display_path: &Path,
    ) -> Result<Option<ManagedEntry>> {
        let value = tree.path_value(repo_path).block_on().with_context(|| {
            format!("failed to load tree value for `{}`", display_path.display())
        })?;
        let materialized =
            materialize_tree_value(repo.store(), repo_path, value, tree.labels()).block_on()?;

        match materialized {
            MaterializedTreeValue::Absent => Ok(None),
            MaterializedTreeValue::File(mut file) => Ok(Some(ManagedEntry::File {
                contents: file.read_all(repo_path).block_on().with_context(|| {
                    format!(
                        "failed to read file content for `{}`",
                        display_path.display()
                    )
                })?,
                executable: file.executable,
            })),
            MaterializedTreeValue::Symlink { target, .. } => Ok(Some(ManagedEntry::Symlink {
                target: PathBuf::from(target),
            })),
            MaterializedTreeValue::FileConflict(file) => {
                let _ = materialize_merge_result_to_bytes(
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
                Ok(Some(ManagedEntry::Conflict))
            }
            MaterializedTreeValue::OtherConflict { .. } => Ok(Some(ManagedEntry::Conflict)),
            MaterializedTreeValue::Tree(_) => Ok(Some(ManagedEntry::Unsupported {
                kind: "directory".to_string(),
            })),
            MaterializedTreeValue::GitSubmodule(_) => Ok(Some(ManagedEntry::Unsupported {
                kind: "git submodule".to_string(),
            })),
            MaterializedTreeValue::AccessDenied(err) => Err(anyhow!(err))
                .with_context(|| format!("access denied reading `{}`", display_path.display())),
        }
    }

    fn revision_summary(
        &self,
        expression: impl Into<String>,
        commit_id: &CommitId,
    ) -> RevisionSummary {
        RevisionSummary::resolved(expression, commit_id.hex())
    }
}

fn resume_state_for_commits(
    base: &RevisionSummary,
    current: &RevisionSummary,
    current_commit: &Commit,
    current_import: &RevisionSummary,
    import_commit: &Commit,
    current_import_has_conflicts: bool,
    current_import_is_descendant_of_base: bool,
    current_is_disposable: bool,
) -> ResumeState {
    if current_import.resolved.is_none() {
        return ResumeState::Blocked {
            reason: format!(
                "`current-import` ({}) is unresolved\n\nresolve the bookmark before rerunning `dotmerge sync`.",
                current_import.short_id()
            ),
        };
    }

    if current_import_has_conflicts {
        return ResumeState::Blocked {
            reason: format!(
                "`current-import` ({}) has unresolved conflicts\n\nresolve the conflicts in `current-import`, then rerun `dotmerge sync`.",
                current_import.short_id()
            ),
        };
    }

    if !current_import_is_descendant_of_base {
        return ResumeState::Blocked {
            reason: format!(
                "`current-import` ({}) is not a descendant of base ({})\n\nit does not look like a dotmerge import on top of the current sync base.\ninspect it with `jj log`, then either:\n  - reset it with `jj bookmark delete current-import`\n  - or move it onto the sync base before rerunning `dotmerge sync`",
                current_import.short_id(),
                base.short_id()
            ),
        };
    }

    if current.same_revision(current_import) {
        return ResumeState::Resumable;
    }

    if import_commit.parent_ids().len() == 1
        && import_commit.parent_ids()[0] == *current_commit.id()
    {
        return ResumeState::Resumable;
    }

    if current_is_disposable {
        if let Some(current_parent) = current_commit.parent_ids().first() {
            if import_commit.parent_ids().len() == 1
                && import_commit.parent_ids()[0] == *current_parent
            {
                return ResumeState::Resumable;
            }
        }
    }

    ResumeState::Blocked {
        reason: format!(
            "`current-import` ({}) does not look like a dotmerge import on top of the current sync base\n\ninspect it with `jj log`, then either:\n  - reset it with `jj bookmark delete current-import`\n  - or move it onto the sync base before rerunning `dotmerge sync`",
            current_import.short_id()
        ),
    }
}

impl JjSession {
    pub(crate) fn repo_mut(&mut self) -> &mut MutableRepo {
        self.tx.repo_mut()
    }

    pub(crate) fn repo(&self) -> &dyn jj_lib::repo::Repo {
        self.tx.repo()
    }

    pub(crate) fn resolve_rev(&self, revset: &str) -> Result<RevisionSummary> {
        let commit = self.resolve_commit_by_revset(revset)?;
        Ok(self.revision_summary(revset, commit.id()))
    }

    pub(crate) fn root_revision(&self) -> Result<RevisionSummary> {
        let root = self.repo().store().root_commit();
        Ok(self.revision_summary("empty-tree", root.id()))
    }

    pub(crate) fn current_revision(&self) -> Result<RevisionSummary> {
        self.resolve_rev("@")
    }

    pub(crate) fn list_files(&self, rev: &RevisionSummary) -> Result<Vec<PathBuf>> {
        let commit = self.resolve_summary_to_commit(rev)?;
        let mut files = commit
            .tree()
            .entries()
            .map(|(path, _)| path.to_fs_path(Path::new("")))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to convert repo paths to filesystem paths")?;
        files.sort();
        Ok(files)
    }

    pub(crate) fn read_entries_at_rev(
        &self,
        rev: &RevisionSummary,
        paths: &BTreeSet<PathBuf>,
    ) -> Result<BTreeMap<PathBuf, Option<ManagedEntry>>> {
        let commit = self.resolve_summary_to_commit(rev)?;
        let tree = commit.tree();
        let mut entries = BTreeMap::new();

        for path in paths {
            let repo_path = self.parse_repo_path(path)?;
            let entry = self.read_entry_from_tree(self.repo(), &tree, repo_path.as_ref(), path)?;
            entries.insert(path.clone(), entry);
        }

        Ok(entries)
    }

    pub(crate) fn complete_sync(&mut self, exported: &RevisionSummary) -> Result<()> {
        let commit = self.resolve_summary_to_commit(exported)?;
        self.tx.repo_mut().set_local_bookmark_target(
            "last-sync".as_ref(),
            RefTarget::normal(commit.id().clone()),
        );
        self.tx
            .repo_mut()
            .set_local_bookmark_target("current-import".as_ref(), RefTarget::absent());
        self.dirty = true;
        Ok(())
    }

    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn resume_state(
        &self,
        base: &RevisionSummary,
        current_import: Option<&RevisionSummary>,
    ) -> Result<ResumeState> {
        let current = self.current_revision()?;
        let current_commit = self.resolve_summary_to_commit(&current)?;
        let current_is_disposable =
            import::is_disposable_sync_placeholder(self.repo(), &current_commit)?;

        match current_import {
            None => Ok(ResumeState::Fresh),
            Some(current_import) => {
                let import_commit = self.resolve_summary_to_commit(current_import)?;
                let current_import_has_conflicts = self.has_conflicts(current_import)?;
                let current_import_is_descendant_of_base =
                    self.is_ancestor(base, current_import)?;
                Ok(resume_state_for_commits(
                    base,
                    &current,
                    &current_commit,
                    current_import,
                    &import_commit,
                    current_import_has_conflicts,
                    current_import_is_descendant_of_base,
                    current_is_disposable,
                ))
            }
        }
    }

    pub(crate) fn has_conflicts(&self, rev: &RevisionSummary) -> Result<bool> {
        let commit = self.resolve_summary_to_commit(rev)?;
        Ok(commit.has_conflict())
    }

    pub(crate) fn bookmark_summary(&self, name: &str) -> Result<BookmarkSummary> {
        let target = self.repo().view().get_local_bookmark(name.as_ref());
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

    pub(crate) fn is_ancestor(
        &self,
        ancestor: &RevisionSummary,
        descendant: &RevisionSummary,
    ) -> Result<bool> {
        let ancestor_commit = self.resolve_summary_to_commit(ancestor)?;
        let descendant_commit = self.resolve_summary_to_commit(descendant)?;
        self.repo()
            .index()
            .is_ancestor(ancestor_commit.id(), descendant_commit.id())
            .context("failed to query jj ancestry")
    }

    pub(crate) fn is_working_copy_clean(&self) -> Result<bool> {
        let current = self.resolve_commit_by_revset("@")?;
        is_working_copy_clean_with_snapshot(
            &self.settings,
            self.workspace.workspace_root(),
            &current.tree(),
        )
    }

    pub(crate) fn checkout_revision(&mut self, rev: &RevisionSummary) -> Result<()> {
        let commit = self.resolve_summary_to_commit(rev)?;
        self.tx
            .repo_mut()
            .edit(self.workspace.workspace_name().to_owned(), &commit)
            .block_on()
            .context("failed to update the workspace commit")?;
        self.tx
            .repo_mut()
            .rebase_descendants()
            .block_on()
            .context("failed to rebase descendants after updating the workspace commit")?;
        self.pending_checkout = Some(commit);
        self.dirty = true;
        Ok(())
    }

    pub(crate) fn create_new_change(
        &mut self,
        parents: &[RevisionSummary],
        message: &str,
    ) -> Result<RevisionSummary> {
        if parents.is_empty() {
            return Err(anyhow!(
                "create_new_change requires at least one parent revision"
            ));
        }

        let parent_commits = parents
            .iter()
            .map(|parent| self.resolve_summary_to_commit(parent))
            .collect::<Result<Vec<_>>>()?;
        let parent_ids = parent_commits
            .iter()
            .map(|commit| commit.id().clone())
            .collect::<Vec<_>>();
        let tree = merge_commit_trees(self.repo(), &parent_commits)
            .block_on()
            .context("failed to derive initial tree for new change")?;

        let commit = self
            .tx
            .repo_mut()
            .new_commit(parent_ids, tree)
            .set_description(message)
            .write()
            .block_on()
            .context("failed to write new commit")?;
        self.dirty = true;
        Ok(self.revision_summary(commit.id().hex(), commit.id()))
    }

    pub(crate) fn finish(mut self, message: impl Into<String>) -> Result<()> {
        let message = message.into();
        if !self.dirty {
            return Ok(());
        }

        let pending_checkout = self.pending_checkout.take();
        let committed_repo = self.tx.commit(message).block_on()?;
        if let Some(commit) = pending_checkout {
            self.workspace
                .check_out(committed_repo.op_id().clone(), None, &commit)
                .block_on()
                .context("failed to check out prepared revision into the repo working copy")?;
        }
        Ok(())
    }

    pub(crate) fn resolve_summary_to_commit(&self, rev: &RevisionSummary) -> Result<Commit> {
        let repo = self.repo();
        if let Some(hex) = &rev.resolved {
            let commit_id = CommitId::try_from_hex(hex)
                .ok_or_else(|| anyhow!("invalid commit id `{hex}` stored in revision summary"))?;
            return repo
                .store()
                .get_commit(&commit_id)
                .with_context(|| format!("failed to load commit `{hex}`"));
        }
        self.resolve_commit_by_revset(&rev.expression)
    }

    fn resolve_commit_by_revset(&self, revset: &str) -> Result<Commit> {
        let repo = self.repo();
        let extensions = RevsetExtensions::default();
        let expression = self.parse_revset(revset, &extensions)?;
        let symbol_resolver = SymbolResolver::new(repo, extensions.symbol_resolvers());
        let resolved = expression
            .resolve_user_expression(repo, &symbol_resolver)
            .with_context(|| format!("failed to resolve revset `{revset}`"))?;
        let evaluated = resolved
            .evaluate(repo)
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
        extensions: &RevsetExtensions,
    ) -> Result<Arc<UserRevsetExpression>> {
        let mut diagnostics = RevsetDiagnostics::new();
        let aliases = RevsetAliasesMap::new();
        let fileset_aliases = FilesetAliasesMap::new();
        let ui = RepoPathUiConverter::Fs {
            cwd: self.workspace.workspace_root().to_path_buf(),
            base: self.workspace.workspace_root().to_path_buf(),
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
                workspace_name: self.workspace.workspace_name(),
            }),
        };
        parse(&mut diagnostics, revset, &context)
            .with_context(|| format!("failed to parse revset `{revset}`"))
    }

    pub(crate) fn parse_repo_path(&self, path: &Path) -> Result<RepoPathBuf> {
        RepoPathBuf::parse_fs_path(
            self.workspace.workspace_root(),
            self.workspace.workspace_root(),
            path,
        )
        .map_err(|err| {
            anyhow!(
                "failed to interpret `{}` as a repo-relative path: {err}",
                path.display()
            )
        })
    }

    fn read_entry_from_tree(
        &self,
        repo: &dyn jj_lib::repo::Repo,
        tree: &jj_lib::merged_tree::MergedTree,
        repo_path: &RepoPath,
        display_path: &Path,
    ) -> Result<Option<ManagedEntry>> {
        let value = tree.path_value(repo_path).block_on().with_context(|| {
            format!("failed to load tree value for `{}`", display_path.display())
        })?;
        let materialized =
            materialize_tree_value(repo.store(), repo_path, value, tree.labels()).block_on()?;

        match materialized {
            MaterializedTreeValue::Absent => Ok(None),
            MaterializedTreeValue::File(mut file) => Ok(Some(ManagedEntry::File {
                contents: file.read_all(repo_path).block_on().with_context(|| {
                    format!(
                        "failed to read file content for `{}`",
                        display_path.display()
                    )
                })?,
                executable: file.executable,
            })),
            MaterializedTreeValue::Symlink { target, .. } => Ok(Some(ManagedEntry::Symlink {
                target: PathBuf::from(target),
            })),
            MaterializedTreeValue::FileConflict(file) => {
                let _ = materialize_merge_result_to_bytes(
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
                Ok(Some(ManagedEntry::Conflict))
            }
            MaterializedTreeValue::OtherConflict { .. } => Ok(Some(ManagedEntry::Conflict)),
            MaterializedTreeValue::Tree(_) => Ok(Some(ManagedEntry::Unsupported {
                kind: "directory".to_string(),
            })),
            MaterializedTreeValue::GitSubmodule(_) => Ok(Some(ManagedEntry::Unsupported {
                kind: "git submodule".to_string(),
            })),
            MaterializedTreeValue::AccessDenied(err) => Err(anyhow!(err))
                .with_context(|| format!("access denied reading `{}`", display_path.display())),
        }
    }

    fn revision_summary(
        &self,
        expression: impl Into<String>,
        commit_id: &CommitId,
    ) -> RevisionSummary {
        RevisionSummary::resolved(expression, commit_id.hex())
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

fn is_working_copy_clean_with_snapshot(
    settings: &UserSettings,
    workspace_root: &Path,
    current_tree: &jj_lib::merged_tree::MergedTree,
) -> Result<bool> {
    let mut workspace = Workspace::load(
        settings,
        workspace_root,
        &StoreFactories::default(),
        &default_working_copy_factories(),
    )
    .with_context(|| {
        format!(
            "failed to load jj workspace at `{}` for working-copy snapshot",
            workspace_root.display()
        )
    })?;
    let mut locked_workspace = workspace
        .start_working_copy_mutation()
        .block_on()
        .context("failed to start jj working-copy snapshot")?;
    let snapshot_options = SnapshotOptions {
        base_ignores: GitIgnoreFile::empty(),
        progress: None,
        start_tracking_matcher: &EverythingMatcher,
        force_tracking_matcher: &NothingMatcher,
        max_new_file_size: u64::MAX,
    };
    let (snapshot_tree, _) = locked_workspace
        .locked_wc()
        .snapshot(&snapshot_options)
        .block_on()
        .context("failed to snapshot jj working copy")?;
    Ok(snapshot_tree.tree_ids_and_labels() == current_tree.tree_ids_and_labels())
}
