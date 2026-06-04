use crate::import;
use crate::jj::JjSession;
use crate::model::{MergeOutcome, Revision};
use crate::util;
use anyhow::Result;
use jj_lib::object_id::ObjectId as _;

pub(crate) fn merge_revisions(
    session: &mut JjSession,
    left: &Revision,
    right: &Revision,
) -> Result<MergeOutcome> {
    let current = session.current_revision()?;
    let current_commit = session.resolve_revision_to_commit(&current)?;
    let (left, right) = import::normalize_disposable_current_in_merge_inputs(
        session.repo(),
        &current,
        &current_commit,
        left,
        right,
    )?;

    if session.is_ancestor(&right, &left)? {
        return Ok(MergeOutcome::NoOp { revision: left });
    }
    if session.is_ancestor(&left, &right)? {
        return Ok(MergeOutcome::FastForward { revision: right });
    }

    let merge_description = merge_description_for_target(session, &right)?;
    let revision = session.create_new_change(&[left, right], &merge_description)?;
    Ok(MergeOutcome::Merged { revision })
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

pub(crate) fn target_label(session: &JjSession, target: &Revision) -> Result<String> {
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
