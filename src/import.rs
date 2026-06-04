use crate::fs;
use crate::jj::JjSession;
use crate::model::{ManagedEntry, RevisionSummary};
use crate::util;
use anyhow::{anyhow, Context, Result};
use jj_lib::backend::TreeId;
use jj_lib::backend::{CopyId, TreeValue};
use jj_lib::commit::Commit;
use jj_lib::merged_tree::MergedTree;
use jj_lib::object_id::ObjectId as _;
use jj_lib::op_store::RefTarget;
use jj_lib::tree_builder::TreeBuilder;
use pollster::FutureExt as _;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ImportPlacement {
    ReuseRepoSide,
    RewriteInPlace(Commit),
    ReplaceOnDisposableParent,
    ReplaceOnCurrent,
}

pub(crate) fn create_or_refresh_import(
    session: &mut JjSession,
    base: &RevisionSummary,
    home_state: &Path,
    managed_paths: &BTreeSet<PathBuf>,
) -> Result<RevisionSummary> {
    let current = session.current_revision()?;
    let current_commit = session.resolve_summary_to_commit(&current)?;
    let current_import = session.bookmark_summary("current-import")?;
    let base_commit = session.resolve_summary_to_commit(base)?;

    let (current_commit, current_import, current_tree_id, imported_tree_id) = {
        let repo = session.repo();
        let base_tree_id = base_commit
            .tree_ids()
            .as_resolved()
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "base revision `{}` must have a resolved tree",
                    base.expression
                )
            })?;

        let mut builder = TreeBuilder::new(repo.store().clone(), base_tree_id);
        for path in managed_paths {
            let repo_path = session.parse_repo_path(path)?;
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

        (
            current_commit,
            current_import,
            current_tree_id,
            imported_tree_id,
        )
    };

    let imported_tree =
        MergedTree::resolved(session.repo().store().clone(), imported_tree_id.clone());
    let current_import_commit = current_import
        .revision
        .as_ref()
        .map(|revision| session.resolve_summary_to_commit(revision))
        .transpose()?;
    let placement = decide_import_placement(
        session.repo(),
        &current_commit,
        current_import_commit.as_ref(),
        &imported_tree_id,
        &current_tree_id,
    )?;

    let import_description = import_description();
    let commit = match placement {
        ImportPlacement::ReuseRepoSide => {
            if current_import.exists {
                session
                    .repo_mut()
                    .set_local_bookmark_target("current-import".as_ref(), RefTarget::absent());
                session.mark_dirty();
            }
            return Ok(current);
        }
        ImportPlacement::RewriteInPlace(import_commit) => {
            let commit = session
                .repo_mut()
                .rewrite_commit(&import_commit)
                .set_parents(vec![current_commit.id().clone()])
                .set_tree(imported_tree)
                .set_description(import_description.clone())
                .write()
                .block_on()
                .context("failed to rewrite imported commit")?;
            session
                .repo_mut()
                .rebase_descendants()
                .block_on()
                .context("failed to rebase descendants after refreshing current-import")?;
            commit
        }
        ImportPlacement::ReplaceOnDisposableParent => session
            .repo_mut()
            .new_commit(current_commit.parent_ids().to_vec(), imported_tree)
            .set_description(import_description.clone())
            .write()
            .block_on()
            .context("failed to create imported commit on top of disposable `@` parent")?,
        ImportPlacement::ReplaceOnCurrent => session
            .repo_mut()
            .new_commit(vec![current_commit.id().clone()], imported_tree)
            .set_description(import_description)
            .write()
            .block_on()
            .context("failed to write imported commit")?,
    };
    session.repo_mut().set_local_bookmark_target(
        "current-import".as_ref(),
        RefTarget::normal(commit.id().clone()),
    );
    session.mark_dirty();
    Ok(RevisionSummary::resolved(
        "current-import",
        commit.id().hex(),
    ))
}

pub(crate) fn decide_import_placement(
    repo: &dyn jj_lib::repo::Repo,
    current_commit: &Commit,
    current_import: Option<&Commit>,
    imported_tree_id: &TreeId,
    current_tree_id: &TreeId,
) -> Result<ImportPlacement> {
    if imported_tree_id == current_tree_id {
        return Ok(ImportPlacement::ReuseRepoSide);
    }

    if let Some(import_commit) = current_import {
        if is_direct_child_of(import_commit, current_commit) {
            return Ok(ImportPlacement::RewriteInPlace(import_commit.clone()));
        }
    }

    if is_disposable_sync_placeholder(repo, current_commit)? {
        return Ok(ImportPlacement::ReplaceOnDisposableParent);
    }

    Ok(ImportPlacement::ReplaceOnCurrent)
}

pub(crate) fn is_disposable_sync_placeholder(
    repo: &dyn jj_lib::repo::Repo,
    commit: &Commit,
) -> Result<bool> {
    if commit.parent_ids().len() != 1 || !commit.description().is_empty() {
        return Ok(false);
    }
    commit
        .is_empty(repo)
        .block_on()
        .context("failed to determine whether current `@` is empty")
}

pub(crate) fn normalize_disposable_current_in_merge_inputs(
    repo: &dyn jj_lib::repo::Repo,
    current: &RevisionSummary,
    current_commit: &Commit,
    left: &RevisionSummary,
    right: &RevisionSummary,
) -> Result<(RevisionSummary, RevisionSummary)> {
    if !is_disposable_sync_placeholder(repo, current_commit)? {
        return Ok((left.clone(), right.clone()));
    }
    if left.same_revision(current) && right.same_revision(current) {
        return Ok((left.clone(), right.clone()));
    }

    let parent_id = current_commit
        .parent_ids()
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("disposable current `@` must have exactly one parent"))?;
    let parent_hex = parent_id.hex();
    let parent = RevisionSummary::resolved(parent_hex.clone(), parent_hex);

    let left = if left.same_revision(current) {
        parent.clone()
    } else {
        left.clone()
    };
    let right = if right.same_revision(current) {
        parent
    } else {
        right.clone()
    };
    Ok((left, right))
}

pub(crate) fn import_description() -> String {
    format!("dotmerge: import changes from {}", util::hostname_label())
}

fn is_direct_child_of(child: &Commit, parent: &Commit) -> bool {
    child.parent_ids() == [parent.id().clone()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use jj_lib::backend::CommitId;
    use jj_lib::config::StackedConfig;
    use jj_lib::merged_tree::MergedTree;
    use jj_lib::repo::Repo;
    use jj_lib::repo::ReadonlyRepo;
    use jj_lib::repo_path::RepoPathBuf;
    use jj_lib::settings::UserSettings;
    use jj_lib::simple_backend::SimpleBackend;
    use jj_lib::transaction::Transaction;
    use jj_lib::tree_builder::TreeBuilder;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

    struct TestRepo {
        _tempdir: TempDir,
        repo: Arc<ReadonlyRepo>,
    }

    impl TestRepo {
        fn init() -> Result<Self> {
            let tempdir = TempDir::new().context("failed to create tempdir")?;
            let repo_path = tempdir.path().join("repo");
            std::fs::create_dir_all(&repo_path)
                .with_context(|| format!("failed to create `{}`", repo_path.display()))?;
            let settings = UserSettings::from_config(StackedConfig::with_defaults())
                .context("failed to build jj settings")?;
            let signer = jj_lib::signing::Signer::from_settings(&settings)
                .context("failed to build signer")?;
            let repo = pollster::block_on(ReadonlyRepo::init(
                &settings,
                &repo_path,
                &|_settings, store_path| Ok(Box::new(SimpleBackend::init(store_path))),
                signer,
                ReadonlyRepo::default_op_store_initializer(),
                ReadonlyRepo::default_op_heads_store_initializer(),
                ReadonlyRepo::default_index_store_initializer(),
                ReadonlyRepo::default_submodule_store_initializer(),
            ))
            .context("failed to initialize test repo")?;
            Ok(Self {
                _tempdir: tempdir,
                repo,
            })
        }

        fn root(&self) -> &Path {
            self._tempdir.path()
        }
    }

    fn root_tree(repo: &Arc<ReadonlyRepo>) -> MergedTree {
        repo.store().root_commit().tree()
    }

    fn tree_with_file(
        repo: &Arc<ReadonlyRepo>,
        root: &Path,
        path: &str,
        contents: &[u8],
    ) -> Result<MergedTree> {
        let repo_path = RepoPathBuf::parse_fs_path(root, root, Path::new(path))
            .with_context(|| format!("failed to parse repo path `{path}`"))?;
        let base_tree_id = repo
            .store()
            .root_commit()
            .tree_ids()
            .as_resolved()
            .cloned()
            .ok_or_else(|| anyhow!("root commit must have a resolved tree"))?;
        let mut builder = TreeBuilder::new(repo.store().clone(), base_tree_id);
        let mut contents = contents;
        let file_id = repo
            .store()
            .write_file(repo_path.as_ref(), &mut contents)
            .block_on()
            .with_context(|| format!("failed to write file contents for `{path}`"))?;
        builder.set(
            repo_path,
            TreeValue::File {
                id: file_id,
                executable: false,
                copy_id: CopyId::placeholder(),
            },
        );
        let tree_id = builder
            .write_tree()
            .block_on()
            .context("failed to write test tree")?;
        Ok(MergedTree::resolved(repo.store().clone(), tree_id))
    }

    fn new_commit(
        tx: &mut Transaction,
        parents: Vec<CommitId>,
        tree: MergedTree,
        description: &str,
    ) -> Result<Commit> {
        tx.repo_mut()
            .new_commit(parents, tree)
            .set_description(description)
            .write()
            .block_on()
            .context("failed to write test commit")
    }

    #[test]
    fn disposable_placeholder_requires_empty_single_parent_commit() -> Result<()> {
        let fixture = TestRepo::init()?;
        let root_id = fixture.repo.store().root_commit_id().clone();

        let mut tx = fixture.repo.start_transaction();
        let disposable = new_commit(&mut tx, vec![root_id.clone()], root_tree(&fixture.repo), "")?;
        assert!(is_disposable_sync_placeholder(
            fixture.repo.as_ref(),
            &disposable
        )?);

        let non_empty_description =
            new_commit(&mut tx, vec![root_id], root_tree(&fixture.repo), "keep me")?;
        assert!(!is_disposable_sync_placeholder(
            fixture.repo.as_ref(),
            &non_empty_description
        )?);

        Ok(())
    }

    #[test]
    fn normalize_disposable_current_replaces_current_with_parent() -> Result<()> {
        let fixture = TestRepo::init()?;
        let root_id = fixture.repo.store().root_commit_id().clone();
        let parent_hex = root_id.hex();
        let parent_summary = RevisionSummary::resolved(parent_hex.clone(), parent_hex);

        let mut tx = fixture.repo.start_transaction();
        let current = new_commit(&mut tx, vec![root_id], root_tree(&fixture.repo), "")?;

        let left = RevisionSummary::resolved("@", current.id().hex());
        let right = RevisionSummary::resolved("other", "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");

        let (normalized_left, normalized_right) = normalize_disposable_current_in_merge_inputs(
            fixture.repo.as_ref(),
            &left,
            &current,
            &left,
            &right,
        )?;

        assert_eq!(normalized_left, parent_summary);
        assert_eq!(normalized_right, right);
        Ok(())
    }

    #[test]
    fn import_placement_reuses_repo_side_when_import_matches_current() -> Result<()> {
        let fixture = TestRepo::init()?;
        let root_id = fixture.repo.store().root_commit_id().clone();

        let mut tx = fixture.repo.start_transaction();
        let current = new_commit(&mut tx, vec![root_id], root_tree(&fixture.repo), "current")?;
        let current_import = new_commit(
            &mut tx,
            vec![current.id().clone()],
            root_tree(&fixture.repo),
            "current-import",
        )?;
        let imported_tree_id = current
            .tree_ids()
            .as_resolved()
            .cloned()
            .context("current commit must have a resolved tree")?;
        let placement = decide_import_placement(
            fixture.repo.as_ref(),
            &current,
            Some(&current_import),
            &imported_tree_id,
            &imported_tree_id,
        )?;
        assert!(matches!(placement, ImportPlacement::ReuseRepoSide));
        Ok(())
    }

    #[test]
    fn import_placement_rewrites_in_place_for_redundant_current_import() -> Result<()> {
        let fixture = TestRepo::init()?;
        let root_id = fixture.repo.store().root_commit_id().clone();
        let imported_tree = tree_with_file(&fixture.repo, fixture.root(), "managed.txt", b"v1")?;

        let mut tx = fixture.repo.start_transaction();
        let current = new_commit(&mut tx, vec![root_id], root_tree(&fixture.repo), "current")?;
        let current_import = new_commit(
            &mut tx,
            vec![current.id().clone()],
            imported_tree.clone(),
            "current-import",
        )?;
        let imported_tree_id = imported_tree
            .tree_ids()
            .as_resolved()
            .cloned()
            .context("imported tree must be resolved")?;
        let current_tree_id = current
            .tree_ids()
            .as_resolved()
            .cloned()
            .context("current tree must be resolved")?;

        let placement = decide_import_placement(
            fixture.repo.as_ref(),
            &current,
            Some(&current_import),
            &imported_tree_id,
            &current_tree_id,
        )?;
        assert!(matches!(
            placement,
            ImportPlacement::RewriteInPlace(ref commit) if commit.id() == current_import.id()
        ));
        Ok(())
    }

    #[test]
    fn import_placement_replaces_on_disposable_parent() -> Result<()> {
        let fixture = TestRepo::init()?;
        let root_id = fixture.repo.store().root_commit_id().clone();
        let imported_tree = tree_with_file(&fixture.repo, fixture.root(), "managed.txt", b"v1")?;

        let mut tx = fixture.repo.start_transaction();
        let current = new_commit(&mut tx, vec![root_id], root_tree(&fixture.repo), "")?;
        let imported_tree_id = imported_tree
            .tree_ids()
            .as_resolved()
            .cloned()
            .context("imported tree must be resolved")?;
        let current_tree_id = current
            .tree_ids()
            .as_resolved()
            .cloned()
            .context("current tree must be resolved")?;

        let placement = decide_import_placement(
            fixture.repo.as_ref(),
            &current,
            None,
            &imported_tree_id,
            &current_tree_id,
        )?;
        assert!(matches!(
            placement,
            ImportPlacement::ReplaceOnDisposableParent
        ));
        Ok(())
    }

    #[test]
    fn import_placement_replaces_on_current_when_current_is_regular() -> Result<()> {
        let fixture = TestRepo::init()?;
        let root_id = fixture.repo.store().root_commit_id().clone();
        let imported_tree = tree_with_file(&fixture.repo, fixture.root(), "managed.txt", b"v1")?;

        let mut tx = fixture.repo.start_transaction();
        let current = new_commit(&mut tx, vec![root_id], root_tree(&fixture.repo), "current")?;
        let imported_tree_id = imported_tree
            .tree_ids()
            .as_resolved()
            .cloned()
            .context("imported tree must be resolved")?;
        let current_tree_id = current
            .tree_ids()
            .as_resolved()
            .cloned()
            .context("current tree must be resolved")?;

        let placement = decide_import_placement(
            fixture.repo.as_ref(),
            &current,
            None,
            &imported_tree_id,
            &current_tree_id,
        )?;
        assert!(matches!(placement, ImportPlacement::ReplaceOnCurrent));
        Ok(())
    }
}
