use crate::import;
use crate::jj::JjSession;
use crate::model::Revision;
use crate::util;
use anyhow::Result;
use jj_lib::commit::Commit;
use jj_lib::object_id::ObjectId as _;

pub(crate) fn merge_revisions(
    session: &mut JjSession,
    left: &Revision,
    right: &Revision,
) -> Result<Revision> {
    let current = session.current_revision()?;
    let current_commit = session.resolve_revision_to_commit(&current)?;
    let (left, right) = normalize_disposable_current_in_merge_inputs(
        session.repo(),
        &current,
        &current_commit,
        left,
        right,
    )?;

    if session.is_ancestor(&right, &left)? {
        return Ok(left);
    }
    if session.is_ancestor(&left, &right)? {
        return Ok(right);
    }

    let merge_description = merge_description_for_target(session, &right)?;
    session.create_new_change(&[left, right], &merge_description)
}

pub(crate) fn normalize_disposable_current_in_merge_inputs(
    repo: &dyn jj_lib::repo::Repo,
    current: &Revision,
    current_commit: &Commit,
    left: &Revision,
    right: &Revision,
) -> Result<(Revision, Revision)> {
    import::normalize_disposable_current_in_merge_inputs(repo, current, current_commit, left, right)
}

pub(crate) fn merge_description_for_target(
    session: &JjSession,
    target: &Revision,
) -> Result<String> {
    Ok(format!(
        "dotmerge: merge {} changes into {}",
        util::hostname_label(),
        target_label(session, target)?
    ))
}

fn target_label(session: &JjSession, target: &Revision) -> Result<String> {
    let commit = session.resolve_revision_to_commit(target)?;
    let mut names = Vec::new();
    for (name, _) in session
        .repo()
        .view()
        .local_bookmarks_for_commit(commit.id())
    {
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
