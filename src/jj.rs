use crate::fs;
use crate::model::{BookmarkSummary, ManagedEntry, RevisionSummary};
use crate::util;
use anyhow::{Context, Result, anyhow};
use chrono::Local;
use jj_lib::backend::{CommitId, CopyId, TreeValue};
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
use jj_lib::repo_path::{RepoPath, RepoPathBuf, RepoPathUiConverter};
use jj_lib::revset::{
    RevsetAliasesMap, RevsetDiagnostics, RevsetExtensions, RevsetParseContext,
    RevsetWorkspaceContext, SymbolResolver, UserRevsetExpression, parse,
};
use jj_lib::rewrite::merge_commit_trees;
use jj_lib::settings::UserSettings;
use jj_lib::time_util::DatePatternContext;
use jj_lib::tree_builder::TreeBuilder;
use jj_lib::tree_merge::MergeOptions;
use jj_lib::workspace::{Workspace, default_working_copy_factories};
use pollster::FutureExt as _;
use std::collections::{BTreeMap, BTreeSet, HashMap};
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

    pub fn working_copy_path(&self, repo_relative: &Path) -> Result<PathBuf> {
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

    pub fn resolve_rev(&self, revset: &str) -> Result<RevisionSummary> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_commit_by_revset(&workspace, &repo, revset)?;
        Ok(self.revision_summary(revset, commit.id()))
    }

    pub fn root_revision(&self) -> Result<RevisionSummary> {
        let (_, repo) = self.load_workspace_and_repo()?;
        let root = repo.store().root_commit();
        Ok(self.revision_summary("empty-tree", root.id()))
    }

    pub fn current_revision(&self) -> Result<RevisionSummary> {
        self.resolve_rev("@")
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

    pub fn read_entry_at_rev(
        &self,
        rev: &RevisionSummary,
        path: &Path,
    ) -> Result<Option<ManagedEntry>> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_summary_to_commit(&workspace, &repo, rev)?;
        let repo_path = self.parse_repo_path(path)?;
        self.read_entry_from_tree(repo.as_ref(), &commit.tree(), repo_path.as_ref(), path)
            .with_context(|| {
                format!(
                    "failed to read `{}` from revision `{}`",
                    path.display(),
                    self.rev_label(rev)
                )
            })
    }

    pub fn read_entries_at_rev(
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

    pub fn file_at_rev(&self, rev: &RevisionSummary, path: &Path) -> Result<Option<Vec<u8>>> {
        match self.read_entry_at_rev(rev, path)? {
            Some(ManagedEntry::File { contents, .. }) => Ok(Some(contents)),
            Some(ManagedEntry::Symlink { target }) => {
                Ok(Some(target.to_string_lossy().into_owned().into_bytes()))
            }
            Some(ManagedEntry::Conflict) => Err(anyhow!(
                "`{}` has unresolved conflicts in `{}`",
                path.display(),
                self.rev_label(rev)
            )),
            Some(ManagedEntry::Unsupported { kind }) => Err(anyhow!(
                "`{}` is unsupported as `{kind}` in `{}`",
                path.display(),
                self.rev_label(rev)
            )),
            None => Ok(None),
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

    pub fn complete_sync(&self, exported: &RevisionSummary) -> Result<()> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_summary_to_commit(&workspace, &repo, exported)?;
        let mut tx = repo.start_transaction();
        tx.repo_mut().set_local_bookmark_target(
            "last-sync".as_ref(),
            RefTarget::normal(commit.id().clone()),
        );
        tx.repo_mut()
            .set_local_bookmark_target("current-import".as_ref(), RefTarget::absent());
        tx.commit("complete dotmerge sync").block_on()?;
        Ok(())
    }

    pub fn create_or_refresh_import(
        &self,
        base: &RevisionSummary,
        home_state: &Path,
        managed_paths: &BTreeSet<PathBuf>,
    ) -> Result<RevisionSummary> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let base_commit = self.resolve_summary_to_commit(&workspace, &repo, base)?;
        let current_commit = self.resolve_commit_by_revset(&workspace, &repo, "@")?;
        let current_import = self.bookmark_summary("current-import")?;
        let base_tree_id = base_commit
            .tree_ids()
            .as_resolved()
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "base revision `{}` must have a resolved tree",
                    self.rev_label(base)
                )
            })?;

        let mut builder = TreeBuilder::new(repo.store().clone(), base_tree_id);
        for path in managed_paths {
            let repo_path = self.parse_repo_path(path)?;
            match fs::read_rooted_entry(home_state, path)? {
                Some(ManagedEntry::File {
                    contents,
                    executable,
                }) => {
                    let file_id = repo
                        .store()
                        .write_file(repo_path.as_ref(), &mut contents.as_slice())
                        .block_on()
                        .with_context(|| {
                            format!(
                                "failed to write imported file `{}` to the jj store",
                                path.display()
                            )
                        })?;
                    builder.set(
                        repo_path,
                        TreeValue::File {
                            id: file_id,
                            executable,
                            copy_id: CopyId::placeholder(),
                        },
                    );
                }
                Some(ManagedEntry::Symlink { target }) => {
                    let target = target.to_str().ok_or_else(|| {
                        anyhow!("symlink target for `{}` is not valid UTF-8", path.display())
                    })?;
                    let symlink_id = repo
                        .store()
                        .write_symlink(repo_path.as_ref(), target)
                        .block_on()
                        .with_context(|| {
                            format!(
                                "failed to write imported symlink `{}` to the jj store",
                                path.display()
                            )
                        })?;
                    builder.set(repo_path, TreeValue::Symlink(symlink_id));
                }
                Some(ManagedEntry::Conflict) => {
                    return Err(anyhow!(
                        "cannot import unresolved conflict from `$HOME` at `{}`",
                        path.display()
                    ));
                }
                Some(ManagedEntry::Unsupported { kind }) => {
                    return Err(anyhow!(
                        "cannot import `{}` from `$HOME` because it is a {kind}",
                        path.display()
                    ));
                }
                None => builder.remove(repo_path),
            }
        }

        let imported_tree_id = builder
            .write_tree()
            .block_on()
            .context("failed to materialize imported tree")?;

        let current_tree_id = current_commit
            .tree_ids()
            .as_resolved()
            .cloned()
            .ok_or_else(|| anyhow!("current `@` revision must have a resolved tree"))?;

        if imported_tree_id == current_tree_id {
            if current_import.exists {
                let mut tx = repo.start_transaction();
                tx.repo_mut()
                    .set_local_bookmark_target("current-import".as_ref(), RefTarget::absent());
                tx.commit("abandon empty current-import").block_on()?;
            }
            return Ok(self.revision_summary("@", current_commit.id()));
        }

        let imported_tree =
            jj_lib::merged_tree::MergedTree::resolved(repo.store().clone(), imported_tree_id);
        let mut tx = repo.start_transaction();
        let current_is_disposable =
            self.is_disposable_sync_placeholder(repo.as_ref(), &current_commit)?;
        let reusable_import = match current_import.revision.as_ref() {
            Some(revision) => {
                let import_commit = self.resolve_summary_to_commit(&workspace, &repo, revision)?;
                if self.is_direct_child_of(&import_commit, &current_commit) {
                    Some(import_commit)
                } else {
                    None
                }
            }
            None => None,
        };

        let commit = if let Some(import_commit) = reusable_import {
            let commit = tx
                .repo_mut()
                .rewrite_commit(&import_commit)
                .set_parents(vec![current_commit.id().clone()])
                .set_tree(imported_tree)
                .set_description(self.import_description())
                .write()
                .block_on()
                .context("failed to rewrite imported commit")?;
            tx.repo_mut()
                .rebase_descendants()
                .block_on()
                .context("failed to rebase descendants after refreshing current-import")?;
            commit
        } else if current_is_disposable {
            tx.repo_mut()
                .new_commit(current_commit.parent_ids().to_vec(), imported_tree)
                .set_description(self.import_description())
                .write()
                .block_on()
                .context("failed to create imported commit on top of disposable `@` parent")?
        } else {
            tx.repo_mut()
                .new_commit(vec![current_commit.id().clone()], imported_tree)
                .set_description(self.import_description())
                .write()
                .block_on()
                .context("failed to write imported commit")?
        };
        tx.repo_mut().set_local_bookmark_target(
            "current-import".as_ref(),
            RefTarget::normal(commit.id().clone()),
        );
        tx.commit("refresh current-import").block_on()?;
        Ok(self.revision_summary("current-import", commit.id()))
    }

    pub fn merge_revisions(
        &self,
        left: &RevisionSummary,
        right: &RevisionSummary,
    ) -> Result<RevisionSummary> {
        let (left, right) = self.normalize_disposable_current_in_merge_inputs(left, right)?;
        if self.is_ancestor(&right, &left)? {
            return Ok(left);
        }
        if self.is_ancestor(&left, &right)? {
            return Ok(right);
        }
        let merge_description = self.merge_description_for_target(&right)?;
        self.create_merge_change(&[left, right], &merge_description)
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
        let current = self.current_revision()?;
        let tracked_paths = self.list_files(&current)?;
        let repo_paths = fs::list_repo_paths(self.repo_path())?;
        let mut managed_paths = tracked_paths.into_iter().collect::<BTreeSet<_>>();
        managed_paths.extend(repo_paths);

        let current_entries = self.read_entries_at_rev(&current, &managed_paths)?;
        for path in managed_paths {
            let fs_entry = fs::read_rooted_entry(self.repo_path(), &path)?;
            let tree_entry = current_entries.get(&path).cloned().flatten();
            if tree_entry != fs_entry {
                return Ok(false);
            }
        }

        Ok(true)
    }

    pub fn is_ancestor(
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

    fn is_direct_child_of(&self, child: &Commit, parent: &Commit) -> bool {
        child.parent_ids() == [parent.id().clone()]
    }

    fn import_description(&self) -> String {
        format!("dotmerge: import changes from {}", util::hostname_label())
    }

    fn merge_description_for_target(&self, target: &RevisionSummary) -> Result<String> {
        Ok(format!(
            "dotmerge: merge {} changes into {}",
            util::hostname_label(),
            self.target_label(target)?
        ))
    }

    fn target_label(&self, target: &RevisionSummary) -> Result<String> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_summary_to_commit(&workspace, &repo, target)?;
        let mut names = Vec::new();
        for (name, _) in repo.view().local_bookmarks_for_commit(commit.id()) {
            names.push(name.as_str().to_string());
        }
        names.sort();
        names.dedup();
        if names.is_empty() {
            Ok(commit.id().hex()[..8].to_string())
        } else {
            Ok(names.join(", "))
        }
    }

    fn is_disposable_sync_placeholder(&self, repo: &ReadonlyRepo, commit: &Commit) -> Result<bool> {
        if commit.parent_ids().len() != 1 || !commit.description().is_empty() {
            return Ok(false);
        }
        commit
            .is_empty(repo)
            .block_on()
            .context("failed to determine whether current `@` is empty")
    }

    fn normalize_disposable_current_in_merge_inputs(
        &self,
        left: &RevisionSummary,
        right: &RevisionSummary,
    ) -> Result<(RevisionSummary, RevisionSummary)> {
        let (workspace, repo) = self.load_workspace_and_repo()?;
        let current = self.current_revision()?;
        let current_commit = self.resolve_summary_to_commit(&workspace, &repo, &current)?;
        if !self.is_disposable_sync_placeholder(repo.as_ref(), &current_commit)? {
            return Ok((left.clone(), right.clone()));
        }
        if left.same_revision(&current) && right.same_revision(&current) {
            return Ok((left.clone(), right.clone()));
        }

        let parent_id = current_commit
            .parent_ids()
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("disposable current `@` must have exactly one parent"))?;
        let parent = self.revision_summary(parent_id.hex(), &parent_id);

        let left = if left.same_revision(&current) {
            parent.clone()
        } else {
            left.clone()
        };
        let right = if right.same_revision(&current) {
            parent
        } else {
            right.clone()
        };
        Ok((left, right))
    }

    pub fn checkout_revision(&self, rev: &RevisionSummary) -> Result<()> {
        let (mut workspace, repo) = self.load_workspace_and_repo()?;
        let commit = self.resolve_summary_to_commit(&workspace, &repo, rev)?;
        let mut tx = repo.start_transaction();
        tx.repo_mut()
            .edit(workspace.workspace_name().to_owned(), &commit)
            .block_on()
            .context("failed to update the workspace commit")?;
        tx.repo_mut()
            .rebase_descendants()
            .block_on()
            .context("failed to rebase descendants after updating the workspace commit")?;
        let new_repo = tx.commit("update working copy for dotmerge").block_on()?;
        workspace
            .check_out(new_repo.op_id().clone(), None, &commit)
            .block_on()
            .context("failed to check out prepared revision into the repo working copy")?;
        Ok(())
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
        parents: &[RevisionSummary],
        message: &str,
    ) -> Result<RevisionSummary> {
        self.create_new_change(parents, message)
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
