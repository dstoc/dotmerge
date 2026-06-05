use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[test]
fn add_accepts_home_relative_file_and_copies_into_repo_worktree() {
    let sandbox = TestSandbox::new();
    let source = sandbox.home().join(".config/sway/config");
    write_file(&source, "theme = \"local\"\n");

    sandbox.init_repo();

    let mut cmd = sandbox.dotmerge();
    cmd.arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg(".config/sway/config");

    cmd.assert().success();

    assert_eq!(
        fs::read_to_string(sandbox.repo().join(".config/sway/config")).unwrap(),
        "theme = \"local\"\n"
    );
}

#[test]
fn add_rejects_path_outside_home() {
    let sandbox = TestSandbox::new();
    let outside_path = sandbox.root().join("outside.txt");
    write_file(&outside_path, "outside\n");

    sandbox.init_repo();

    let mut cmd = sandbox.dotmerge();
    cmd.arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg(&outside_path);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("resolves outside `$HOME`"));
}

#[test]
fn status_on_initial_repo_reports_missing_last_sync_state() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    let mut cmd = sandbox.dotmerge();
    cmd.arg("status")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());

    cmd.assert().success().stdout(
        predicate::str::contains("import:  none")
            .and(predicate::str::contains("state:   up to date"))
            .and(predicate::str::contains(
                "sync will:  nothing — $HOME, repo, and target already agree",
            ))
            .and(predicate::str::contains("repo working copy is not clean").not()),
    );
}

#[test]
fn status_reports_target_already_applied_without_target_diff_details() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.home().join("foo"), "hello\n");

    let mut add = sandbox.dotmerge();
    add.arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "add foo"]);

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let mut status = sandbox.dotmerge();
    status
        .arg("status")
        .arg("--target")
        .arg("root()")
        .arg("--repo")
        .arg(sandbox.repo());

    status.assert().success().stdout(
        predicate::str::contains("(already applied)")
            .and(predicate::str::contains("incoming changes (target since base)").not())
            .and(predicate::str::contains("  - deleted  foo").not()),
    );
}

#[test]
fn status_and_sync_block_unrelated_current_import_without_moving_bookmarks() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.repo().join("bar"), "remote\n");
    sandbox.run_jj(&["desc", "-m", "target bar"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);

    write_file(&sandbox.home().join("foo"), "local\n");
    let mut add = sandbox.dotmerge();
    add.arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();
    sandbox.run_jj(&["desc", "-m", "add foo"]);

    let mut initial_sync = sandbox.dotmerge();
    initial_sync
        .arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    initial_sync.assert().success();

    write_file(&sandbox.home().join("foo"), "local-2\n");

    let mut prepare_sync = sandbox.dotmerge();
    prepare_sync
        .arg("sync")
        .arg("--no-export")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    prepare_sync.assert().success();

    sandbox.run_jj(&["new", "root()"]);
    sandbox.run_jj(&["bookmark", "set", "current-import", "-r", "@", "-B"]);

    let last_sync_before = sandbox
        .jj_stdout(&["log", "-r", "last-sync", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();
    let current_import_before = sandbox
        .jj_stdout(&["log", "-r", "current-import", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();

    let mut status = sandbox.dotmerge();
    status
        .arg("status")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());

    status.assert().success().stdout(
        predicate::str::contains("`current-import` (")
            .and(predicate::str::contains("is not a descendant of base"))
            .and(predicate::str::contains("inspect it with `jj log`"))
            .and(predicate::str::contains(
                "repair `current-import` until it satisfies the resume preconditions, then rerun `dotmerge sync`",
            )),
    );

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());

    sync.assert().failure().stderr(
        predicate::str::contains("`current-import` (")
            .and(predicate::str::contains("is not a descendant of base"))
            .and(predicate::str::contains("inspect it with `jj log`")),
    );

    let last_sync_after = sandbox
        .jj_stdout(&["log", "-r", "last-sync", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();
    let current_import_after = sandbox
        .jj_stdout(&["log", "-r", "current-import", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();

    assert_eq!(last_sync_before, last_sync_after);
    assert_eq!(current_import_before, current_import_after);
}

#[test]
fn sync_recovers_from_interrupted_import_after_resume_state_validation() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.repo().join("managed/foo"), "shared\n");
    sandbox.run_jj(&["desc", "-m", "target foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);
    sandbox.run_jj(&["new", "root()"]);

    write_file(&sandbox.home().join("managed/foo"), "shared\n");

    let mut initial_sync = sandbox.dotmerge();
    initial_sync
        .arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    initial_sync.assert().success();

    // Drift $HOME, then prepare a merge without exporting (sets current-import).
    write_file(&sandbox.home().join("managed/foo"), "home-2\n");

    let mut prepare_sync = sandbox.dotmerge();
    prepare_sync
        .arg("sync")
        .arg("--no-export")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    prepare_sync.assert().success();

    // Advance origin/main to add managed/bar — a file absent from $HOME.
    // The next full sync must write bar to $HOME, so the export will be
    // attempted.  We restore `@` to current-import afterward so that the
    // interrupted sync sees a clean, Resumable working-copy state.
    sandbox.run_jj(&["new", "origin/main", "-m", "add bar"]);
    write_file(&sandbox.repo().join("managed/bar"), "bar\n");
    sandbox.run_jj(&["bookmark", "move", "origin/main", "--to", "@"]);
    sandbox.run_jj(&["edit", "current-import"]);

    let managed_dir = sandbox.home().join("managed");
    let original_mode = fs::metadata(&managed_dir).unwrap().permissions().mode();
    let readonly_mode = original_mode & !0o222;
    fs::set_permissions(&managed_dir, fs::Permissions::from_mode(readonly_mode)).unwrap();

    let mut interrupted_sync = sandbox.dotmerge();
    interrupted_sync
        .arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    interrupted_sync
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to create temp file"));

    fs::set_permissions(&managed_dir, fs::Permissions::from_mode(original_mode)).unwrap();

    let mut status = sandbox.dotmerge();
    status
        .arg("status")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    status.assert().success().stdout(
        predicate::str::contains("state:   merge prepared")
            .and(predicate::str::contains("import:  current-import"))
            .and(predicate::str::contains(
                "sync will:  export the prepared merge to $HOME and advance last-sync",
            )),
    );

    let mut rerun_sync = sandbox.dotmerge();
    rerun_sync
        .arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    rerun_sync.assert().success();
}

#[test]
fn sync_on_fresh_empty_repo_records_last_sync_and_clears_current_import() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    let mut cmd = sandbox.dotmerge();
    cmd.arg("sync")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());

    cmd.assert().success();

    assert_bookmark_present(sandbox.home(), sandbox.repo(), "last-sync");
    assert_bookmark_absent(sandbox.home(), sandbox.repo(), "current-import");
}

#[test]
fn sync_exports_moved_bookmarks_to_colocated_git_refs() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo_colocated();

    let mut cmd = sandbox.dotmerge();
    cmd.arg("sync")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());

    cmd.assert().success();

    // jj records the bookmark move in its own view, but in a colocated repo
    // the visible git ref under refs/heads/ only changes if we export.
    let jj_target = sandbox
        .jj_stdout(&["log", "-r", "last-sync", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_string();
    let git_target = git_ref_target(sandbox.repo(), "refs/heads/last-sync");
    assert_eq!(
        git_target, jj_target,
        "colocated git ref `refs/heads/last-sync` should match jj's last-sync commit"
    );
}

#[test]
fn sync_conflict_persists_current_import_and_conflicted_merge() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    // Target adds `foo` with one content; $HOME has `foo` with conflicting
    // content. With no last-sync, base is the empty tree, so both sides add
    // `foo` differently and the merge conflicts.
    write_file(&sandbox.repo().join("foo"), "from-target\n");
    sandbox.run_jj(&["desc", "-m", "target foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);
    sandbox.run_jj(&["new", "root()"]);

    write_file(&sandbox.home().join("foo"), "from-home\n");

    let mut cmd = sandbox.dotmerge();
    cmd.arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains(
            "merge produced jj conflicts at `@`",
        ));

    // The conflicted state must be persisted for the user to resolve and rerun,
    // not discarded with the transaction.
    assert_bookmark_present(sandbox.home(), sandbox.repo(), "current-import");
    let at_is_conflict = sandbox
        .jj_stdout(&[
            "log",
            "-r",
            "@",
            "--no-graph",
            "-T",
            "if(conflict, \"yes\", \"no\")",
        ])
        .trim()
        .to_string();
    assert_eq!(
        at_is_conflict, "yes",
        "expected `@` to be left at the conflicted merge"
    );

    // last-sync must not move: it only advances after a clean export.
    assert_bookmark_absent(sandbox.home(), sandbox.repo(), "last-sync");
}

#[test]
fn sync_commits_one_dotmerge_operation_in_op_log() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.repo().join("foo"), "shared\n");
    sandbox.run_jj(&["desc", "-m", "target foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);
    sandbox.run_jj(&["new", "root()"]);

    write_file(&sandbox.home().join("foo"), "shared\n");

    let mut cmd = sandbox.dotmerge();
    cmd.arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());

    cmd.assert().success();

    let op_log = sandbox.jj_stdout(&["op", "log", "-T", "description.first_line() ++ \"\\n\""]);
    let dotmerge_ops = op_log.lines().filter(|line| line.contains("dotmerge")).count();
    assert_eq!(
        dotmerge_ops, 1,
        "expected one jj operation for `dotmerge sync`, got:\n{op_log}"
    );
}

#[test]
fn sync_no_export_on_fresh_empty_repo_does_not_record_last_sync() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    let mut cmd = sandbox.dotmerge();
    cmd.arg("sync")
        .arg("--no-export")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());

    cmd.assert().success();

    assert_bookmark_absent(sandbox.home(), sandbox.repo(), "last-sync");
    assert_bookmark_absent(sandbox.home(), sandbox.repo(), "current-import");
}

#[test]
fn sync_export_failure_leaves_last_sync_and_current_import_unchanged() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.repo().join("managed/foo"), "shared\n");
    sandbox.run_jj(&["desc", "-m", "target foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);
    sandbox.run_jj(&["new", "root()"]);

    write_file(&sandbox.home().join("managed/foo"), "shared\n");

    let mut initial_sync = sandbox.dotmerge();
    initial_sync
        .arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    initial_sync.assert().success();

    // Drift $HOME, then prepare a merge without exporting (sets current-import).
    write_file(&sandbox.home().join("managed/foo"), "home-2\n");

    let mut prepare_sync = sandbox.dotmerge();
    prepare_sync
        .arg("sync")
        .arg("--no-export")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    prepare_sync.assert().success();

    // Advance origin/main to add managed/bar — a file absent from $HOME.
    // The next full sync must write bar to $HOME, so the export will be
    // attempted.  We restore `@` to current-import afterward so that the
    // failing sync sees a clean, Resumable working-copy state.
    sandbox.run_jj(&["new", "origin/main", "-m", "add bar"]);
    write_file(&sandbox.repo().join("managed/bar"), "bar\n");
    sandbox.run_jj(&["bookmark", "move", "origin/main", "--to", "@"]);
    sandbox.run_jj(&["edit", "current-import"]);

    let last_sync_before = sandbox
        .jj_stdout(&["log", "-r", "last-sync", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();
    let current_import_before = sandbox
        .jj_stdout(&["log", "-r", "current-import", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();

    let managed_dir = sandbox.home().join("managed");
    let original_mode = fs::metadata(&managed_dir).unwrap().permissions().mode();
    let readonly_mode = original_mode & !0o222;
    fs::set_permissions(&managed_dir, fs::Permissions::from_mode(readonly_mode)).unwrap();

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());

    sync.assert()
        .failure()
        .stderr(predicate::str::contains("failed to create temp file"));

    fs::set_permissions(&managed_dir, fs::Permissions::from_mode(original_mode)).unwrap();

    let last_sync_after = sandbox
        .jj_stdout(&["log", "-r", "last-sync", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();
    let current_import_after = sandbox
        .jj_stdout(&["log", "-r", "current-import", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();

    assert_eq!(last_sync_before, last_sync_after);
    assert_eq!(current_import_before, current_import_after);
}

#[test]
fn sync_reuses_target_without_preserving_disposable_empty_at() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.repo().join("foo"), "hello\n");
    sandbox.run_jj(&["desc", "-m", "target foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);
    sandbox.run_jj(&["new", "root()"]);

    let disposable_at = sandbox
        .jj_stdout(&["log", "-r", "@", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let ancestry = sandbox.jj_stdout(&[
        "log",
        "-r",
        &format!("{disposable_at}::last-sync"),
        "--no-graph",
        "-T",
        "commit_id ++ \"\\n\"",
    ]);
    assert!(
        !ancestry.lines().any(|line| line.trim() == disposable_at),
        "expected disposable empty `@` not to remain in sync ancestry; ancestry was:\n{ancestry}"
    );
}

#[test]
fn sync_reuses_existing_added_revision_as_import_ancestor() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    sandbox.run_jj(&["new", "root()"]);
    sandbox.run_jj(&["desc", "-m", "empty"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);
    sandbox.run_jj(&["new", "root()"]);

    write_file(&sandbox.home().join("foo"), "hello\n");

    let mut add = sandbox.dotmerge();
    add.arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "adding foo"]);

    let adding_commit = sandbox
        .jj_stdout(&["log", "-r", "@", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();
    assert!(
        !adding_commit.is_empty(),
        "expected an `adding foo` revision"
    );

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let ancestry = sandbox.jj_stdout(&[
        "log",
        "-r",
        &format!("{adding_commit}::last-sync"),
        "--no-graph",
        "-T",
        "commit_id ++ \"\\n\"",
    ]);
    assert!(
        ancestry.lines().any(|line| line.trim() == adding_commit),
        "`adding foo` should be an ancestor of `last-sync`; ancestry was:\n{ancestry}"
    );
}

#[test]
fn sync_reuses_import_without_merge_when_target_is_ancestor() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.home().join("foo"), "hello\n");

    let mut add = sandbox.dotmerge();
    add.arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "adding foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let last_sync_description = sandbox
        .jj_stdout(&[
            "log",
            "-r",
            "last-sync",
            "--no-graph",
            "-T",
            "description.first_line()",
        ])
        .trim()
        .to_owned();
    assert_eq!(
        last_sync_description, "adding foo",
        "expected sync to reuse the existing repo-side state instead of creating a redundant import commit"
    );

    let last_sync_parents = sandbox.jj_stdout(&[
        "log",
        "-r",
        "last-sync",
        "--no-graph",
        "-T",
        "parents.map(|c| c.commit_id()).join(\" \") ++ \"\\n\"",
    ]);
    assert_eq!(
        last_sync_parents.split_whitespace().count(),
        1,
        "expected `last-sync` to be a non-merge commit when target is already an ancestor:\n{last_sync_parents}"
    );
}

#[test]
fn sync_merge_description_uses_hostname_and_target_bookmark() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.repo().join("bar"), "remote\n");
    sandbox.run_jj(&["desc", "-m", "target bar"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);
    sandbox.run_jj(&["new", "root()"]);

    write_file(&sandbox.home().join("foo"), "local\n");
    let mut add = sandbox.dotmerge();
    add.arg("add").arg("--repo").arg(sandbox.repo()).arg("foo");
    add.assert().success();
    sandbox.run_jj(&["desc", "-m", "add foo"]);

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let description = sandbox
        .jj_stdout(&[
            "log",
            "-r",
            "last-sync",
            "--no-graph",
            "-T",
            "description.first_line()",
        ])
        .trim()
        .to_owned();
    // Host comes from gethostname(2); assert the format and that the target
    // label resolves to the bookmark name.
    assert!(
        description.starts_with("dotmerge: merge ")
            && description.ends_with(" changes into origin/main"),
        "unexpected description: {description}"
    );
}

#[test]
fn sync_merge_description_uses_target_short_id_when_unbookmarked() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.repo().join("bar"), "remote\n");
    sandbox.run_jj(&["desc", "-m", "target bar"]);
    let target_short = sandbox
        .jj_stdout(&["log", "-r", "@", "--no-graph", "-T", "commit_id.short()"])
        .trim()
        .chars()
        .take(8)
        .collect::<String>();
    sandbox.run_jj(&["new", "root()"]);

    write_file(&sandbox.home().join("foo"), "local\n");
    let mut add = sandbox.dotmerge();
    add.arg("add").arg("--repo").arg(sandbox.repo()).arg("foo");
    add.assert().success();
    sandbox.run_jj(&["desc", "-m", "add foo"]);

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg(&target_short)
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let description = sandbox
        .jj_stdout(&[
            "log",
            "-r",
            "last-sync",
            "--no-graph",
            "-T",
            "description.first_line()",
        ])
        .trim()
        .to_owned();
    // Host comes from gethostname(2); assert the format and that the target
    // label falls back to the short commit id when unbookmarked.
    assert!(
        description.starts_with("dotmerge: merge ")
            && description.ends_with(&format!(" changes into {target_short}")),
        "unexpected description: {description}"
    );
}

#[test]
fn sync_import_description_includes_host() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.home().join("foo"), "old\n");
    let mut add = sandbox.dotmerge();
    add.arg("add").arg("--repo").arg(sandbox.repo()).arg("foo");
    add.assert().success();
    sandbox.run_jj(&["desc", "-m", "add foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);

    write_file(&sandbox.home().join("foo"), "new\n");
    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--no-export")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let description = sandbox
        .jj_stdout(&[
            "log",
            "-r",
            "current-import",
            "--no-graph",
            "-T",
            "description.first_line()",
        ])
        .trim()
        .to_owned();
    // The host comes from gethostname(2); assert the shape and a real,
    // non-fallback host rather than an injected value.
    let host = description
        .strip_prefix("dotmerge: import changes from ")
        .unwrap_or_else(|| panic!("unexpected description: {description}"));
    assert!(!host.is_empty());
    assert_ne!(host, "unknown host");

    // The author identity must come from jj's config (read via `jj config get`),
    // not an empty "(no email set)" default.
    let author = sandbox
        .jj_stdout(&[
            "log",
            "-r",
            "current-import",
            "--no-graph",
            "-T",
            "author.name() ++ \" <\" ++ author.email() ++ \">\"",
        ])
        .trim()
        .to_owned();
    assert_eq!(author, "Test User <test@example.com>");
}

#[test]
fn sync_stops_managing_target_file_deleted_in_at() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.home().join("foo"), "hello\n");

    let mut add = sandbox.dotmerge();
    add.arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "add foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);

    fs::remove_file(sandbox.repo().join("foo")).unwrap();
    sandbox.run_jj(&["describe", "-m", "remove foo"]);

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let last_sync_files = sandbox.jj_stdout(&["file", "list", "-r", "last-sync"]);
    assert!(
        !last_sync_files.split_whitespace().any(|path| path == "foo"),
        "expected `foo` to stay deleted in `last-sync`, got file paths:\n{last_sync_files}"
    );

    assert!(
        sandbox.home().join("foo").exists(),
        "expected sync to leave unmanaged `$HOME/foo` untouched"
    );
}

#[test]
fn sync_rerun_moves_current_import_after_repo_side_state_before_refresh() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    sandbox.run_jj(&["bookmark", "create", "origin/main"]);

    let mut initial_sync = sandbox.dotmerge();
    initial_sync
        .arg("sync")
        .arg("--no-export")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    initial_sync.assert().success();

    sandbox.run_jj(&["new", "@"]);
    write_file(&sandbox.home().join("foo"), "hello\n");

    let mut add = sandbox.dotmerge();
    add.arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();
    sandbox.run_jj(&["desc", "-m", "adds foo"]);

    let repo_side_before_rerun = sandbox
        .jj_stdout(&["log", "-r", "@", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();
    assert!(
        !repo_side_before_rerun.is_empty(),
        "expected a repo-side commit before rerunning sync"
    );

    let mut rerun_sync = sandbox.dotmerge();
    rerun_sync
        .arg("sync")
        .arg("--no-export")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    rerun_sync.assert().success();

    assert_bookmark_absent(sandbox.home(), sandbox.repo(), "current-import");

    let current_after_rerun = sandbox
        .jj_stdout(&["log", "-r", "@", "--no-graph", "-T", "commit_id"])
        .trim()
        .to_owned();
    assert!(
        current_after_rerun == repo_side_before_rerun,
        "expected rerun sync to reuse the repo-side state unchanged when the import is redundant"
    );
}

#[test]
fn status_succeeds_when_tracked_repo_file_is_unreadable_but_unchanged() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    let source = sandbox.home().join("managed/config.toml");
    write_file(&source, "theme = \"local\"\n");

    let mut add = sandbox.dotmerge();
    add.arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("managed/config.toml");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "add managed config"]);

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let repo_file = sandbox.repo().join("managed/config.toml");
    let original_mode = fs::metadata(&repo_file).unwrap().permissions().mode();
    let unreadable_mode = original_mode & !0o444;
    fs::set_permissions(&repo_file, fs::Permissions::from_mode(unreadable_mode)).unwrap();

    let mut status = sandbox.dotmerge();
    status
        .arg("status")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());
    status.assert().success().stdout(
        predicate::str::contains("repo working copy is not clean").not(),
    );

    fs::set_permissions(&repo_file, fs::Permissions::from_mode(original_mode)).unwrap();
}

#[test]
fn status_reports_repo_dirty_after_first_byte_change_in_large_tracked_file() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    let source = sandbox.home().join("managed/large.bin");
    let original_contents = "0123456789abcdef".repeat(16 * 1024);
    write_file(&source, &original_contents);

    let mut add = sandbox.dotmerge();
    add.arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("managed/large.bin");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "add large tracked file"]);

    let mut sync = sandbox.dotmerge();
    sync.arg("sync")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let repo_file = sandbox.repo().join("managed/large.bin");
    let mut modified_contents = fs::read(&repo_file).unwrap();
    modified_contents[0] ^= 0xff;
    fs::write(&repo_file, modified_contents).unwrap();

    let mut status = sandbox.dotmerge();
    status
        .arg("status")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());
    status.assert().success().stdout(predicate::str::contains(
        "repo working copy is not clean",
    ));
}

// Regression test: dropping LockedWorkspace without calling finish() must
// release the working-copy lock cleanly so a second snapshot (a second
// `dotmerge status` invocation) can acquire the lock without hitting a
// "working copy is locked" error.
#[test]
fn status_repeated_twice_does_not_leave_a_stale_working_copy_lock() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    // Run `dotmerge status` twice in a row on the same repo.
    // Each invocation calls is_working_copy_clean_with_snapshot, which acquires
    // and then drops (without finish()) the LockedLocalWorkingCopy.  If the
    // lock file were not cleaned up on drop the second call would fail.
    for _ in 0..2 {
        let mut cmd = sandbox.dotmerge();
        cmd.arg("status")
            .arg("--target")
            .arg("@")
            .arg("--repo")
            .arg(sandbox.repo());
        cmd.assert()
            .success()
            .stdout(predicate::str::contains("repo working copy is not clean").not());
    }
}

// ---------------------------------------------------------------------------
// Config-file feature tests (proposal 0008)
// ---------------------------------------------------------------------------

/// Bullet 1: config supplies all three (repo, target, home).
/// `dotmerge status` with no flags reads everything from the default config
/// and behaves identically to passing the same values as flags.
#[test]
fn config_all_three_fields_status_runs_without_flags() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    // Write the config at the default path ($HOME/.config/dotmerge/config.toml).
    let cfg_path = sandbox.default_config_path();
    write_file(
        &cfg_path,
        &format!(
            "repo = \"{}\"\ntarget = \"@\"\n",
            sandbox.repo().display()
        ),
    );

    // Without any --repo/--target flags the config should supply both.
    sandbox
        .dotmerge()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("state:   up to date"));
}

/// Bullet 2: --repo and --target flags override the corresponding config values.
#[test]
fn config_flags_override_config_values() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    // Write a config pointing at a nonexistent (bogus) repo and a bogus target.
    let cfg_path = sandbox.default_config_path();
    write_file(
        &cfg_path,
        "repo = \"/nonexistent-bogus-repo\"\ntarget = \"bogus-target\"\n",
    );

    // The --repo and --target flags should win over the config values.
    sandbox
        .dotmerge()
        .arg("status")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("--target")
        .arg("@")
        .assert()
        .success()
        .stdout(predicate::str::contains("state:   up to date"));
}

/// Bullet 3a: config with only `repo`; `dotmerge status` without `--target` errors.
#[test]
fn config_repo_only_status_requires_target_flag() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    let cfg_path = sandbox.default_config_path();
    write_file(
        &cfg_path,
        &format!("repo = \"{}\"\n", sandbox.repo().display()),
    );

    sandbox
        .dotmerge()
        .arg("status")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--target is required"));
}

/// Bullet 3b: config with only `repo`; `dotmerge add` succeeds (add needs no target).
#[test]
fn config_repo_only_add_succeeds() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.home().join("foo"), "hello\n");

    let cfg_path = sandbox.default_config_path();
    write_file(
        &cfg_path,
        &format!("repo = \"{}\"\n", sandbox.repo().display()),
    );

    // add uses the configured repo and no target — should succeed.
    sandbox
        .dotmerge()
        .arg("add")
        .arg("foo")
        .assert()
        .success();

    assert!(
        sandbox.repo().join("foo").exists(),
        "expected `foo` to be copied into the repo"
    );
}

/// Bullet 4: no config, no --repo → errors with `--repo is required`.
#[test]
fn config_missing_repo_errors() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    // No config file, no --repo flag.
    sandbox
        .dotmerge()
        .arg("status")
        .arg("--target")
        .arg("@")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--repo is required"));
}

/// Bullet 5a: `--config` pointing at a missing file errors.
#[test]
fn config_flag_missing_path_errors() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    sandbox
        .dotmerge()
        .arg("--config")
        .arg("/tmp/dotmerge-test-missing-config-99999.toml")
        .arg("status")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("--target")
        .arg("@")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--config path does not exist"));
}

/// Bullet 5b: `DOTMERGE_CONFIG` pointing at a missing file errors.
#[test]
fn config_env_missing_path_errors() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    sandbox
        .dotmerge()
        .env("DOTMERGE_CONFIG", "/tmp/dotmerge-test-missing-env-99999.toml")
        .arg("status")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("--target")
        .arg("@")
        .assert()
        .failure()
        .stderr(predicate::str::contains("DOTMERGE_CONFIG path does not exist"));
}

/// Bullet 5c: default path missing is fine — existing tests already cover this
/// (any test that passes --repo/--target without a config file), but this makes
/// the no-config case explicit.
#[test]
fn config_default_path_absent_is_not_an_error() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    // No config file exists; --repo and --target supplied via flags.
    sandbox
        .dotmerge()
        .arg("status")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("--target")
        .arg("@")
        .assert()
        .success();
}

/// Bullet 6: unknown key in the TOML (via --config) errors.
#[test]
fn config_unknown_key_errors() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    // Write a config with a typo'd key; deny_unknown_fields must reject it.
    let cfg_path = sandbox.root().join("bad_config.toml");
    write_file(&cfg_path, "tagret = \"origin/main\"\n");

    sandbox
        .dotmerge()
        .arg("--config")
        .arg(&cfg_path)
        .arg("status")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("--target")
        .arg("@")
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to parse config file"));
}

/// Bullet 7: relative path in config `repo` errors with the "absolute or start with `~/`" message.
#[test]
fn config_relative_repo_path_errors() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    let cfg_path = sandbox.root().join("relative_config.toml");
    write_file(&cfg_path, "repo = \"relative/path\"\ntarget = \"@\"\n");

    sandbox
        .dotmerge()
        .arg("--config")
        .arg(&cfg_path)
        .arg("status")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "paths must be absolute or start with `~/`",
        ));
}

/// Bullet 8: `~/` expansion in config — repo path starting with `~/` is
/// resolved against the real $HOME (the sandbox home).
/// The sandbox repo lives at `<tempdir>/repo`; since it is not under home we
/// instead pass repo via `--repo` and exercise `~/` expansion via the `--repo`
/// flag itself, which goes through the same `expand_path` code path.
#[test]
fn config_tilde_expansion_in_repo_path() {
    let sandbox = TestSandbox::new();

    // Place the repo inside the sandbox home so we can refer to it as ~/…
    let repo_under_home = sandbox.home().join("myrepo");
    let mut init = Command::new("jj");
    init.current_dir(sandbox.root())
        .env("HOME", sandbox.home())
        .arg("git")
        .arg("init")
        .arg(&repo_under_home);
    init.assert().success();

    // Write a config whose `repo` uses `~/myrepo` (tilde-relative to $HOME).
    let cfg_path = sandbox.root().join("tilde_config.toml");
    write_file(&cfg_path, "repo = \"~/myrepo\"\ntarget = \"@\"\n");

    // dotmerge should expand `~/myrepo` against the sandbox $HOME.
    sandbox
        .dotmerge()
        .arg("--config")
        .arg(&cfg_path)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("state:   up to date"));
}

/// Bullet 9: `--config` flag takes precedence over `DOTMERGE_CONFIG` env var.
#[test]
fn config_flag_wins_over_env_var() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    // flag_config: provides repo + target "@" (valid).
    let flag_cfg = sandbox.root().join("flag_config.toml");
    write_file(
        &flag_cfg,
        &format!(
            "repo = \"{}\"\ntarget = \"@\"\n",
            sandbox.repo().display()
        ),
    );

    // env_config: provides a bogus repo so that if it is used the command will fail.
    let env_cfg = sandbox.root().join("env_config.toml");
    write_file(
        &env_cfg,
        "repo = \"/nonexistent-bogus-for-env-config\"\ntarget = \"@\"\n",
    );

    // With --config pointing at the valid config, the env var's bogus config must be ignored.
    sandbox
        .dotmerge()
        .env("DOTMERGE_CONFIG", &env_cfg)
        .arg("--config")
        .arg(&flag_cfg)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("state:   up to date"));
}

struct TestSandbox {
    tempdir: TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl TestSandbox {
    fn new() -> Self {
        let tempdir = TempDir::new().unwrap();
        let home = tempdir.path().join("home");
        let repo = tempdir.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        // Give jj an author identity at the HOME level so every repo under this
        // sandbox resolves one — dotmerge now reads it via `jj config get`, and
        // refuses to author commits without it.
        write_file(
            &home.join(".config/jj/config.toml"),
            "[user]\nname = \"Test User\"\nemail = \"test@example.com\"\n",
        );
        Self {
            tempdir,
            home,
            repo,
        }
    }

    fn root(&self) -> &Path {
        self.tempdir.path()
    }

    /// Return the default config-file path relative to the sandbox home.
    /// Matches the default-path logic in `src/config.rs`: `$HOME/.config/dotmerge/config.toml`.
    fn default_config_path(&self) -> PathBuf {
        self.home.join(".config").join("dotmerge").join("config.toml")
    }

    fn home(&self) -> &Path {
        &self.home
    }

    fn repo(&self) -> &Path {
        &self.repo
    }

    /// Return a `Command` for the `dotmerge` binary with the sandbox `$HOME`
    /// pre-set and the two config-discovery env vars scrubbed so that a real
    /// `~/.config/dotmerge/config.toml` or `DOTMERGE_CONFIG` in the test
    /// runner's environment cannot leak into the invocation.
    fn dotmerge(&self) -> Command {
        let mut cmd = Command::cargo_bin("dotmerge").unwrap();
        cmd.env("HOME", self.home())
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("DOTMERGE_CONFIG");
        cmd
    }

    fn init_repo(&self) {
        let mut cmd = Command::new("jj");
        cmd.current_dir(self.root())
            .env("HOME", self.home())
            .arg("git")
            .arg("init")
            .arg(self.repo());
        cmd.assert().success();
    }

    fn init_repo_colocated(&self) {
        let mut cmd = Command::new("jj");
        cmd.current_dir(self.root())
            .env("HOME", self.home())
            .arg("git")
            .arg("init")
            .arg("--colocate")
            .arg(self.repo());
        cmd.assert().success();
    }

    fn run_jj(&self, args: &[&str]) {
        let mut cmd = Command::new("jj");
        cmd.current_dir(self.repo())
            .env("HOME", self.home())
            .args(args);
        cmd.assert().success();
    }

    fn jj_stdout(&self, args: &[&str]) -> String {
        let output = std::process::Command::new("jj")
            .current_dir(self.repo())
            .env("HOME", self.home())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "jj {:?} failed: stdout=\n{}\nstderr=\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn git_ref_target(repo: &Path, ref_name: &str) -> String {
    let output = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(repo.join(".git"))
        .arg("rev-parse")
        .arg("--verify")
        .arg(ref_name)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git rev-parse {ref_name} failed: stderr=\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn assert_bookmark_present(home: &Path, repo: &Path, name: &str) {
    let mut cmd = Command::new("jj");
    cmd.current_dir(repo)
        .env("HOME", home)
        .arg("bookmark")
        .arg("list")
        .arg(name);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains(format!("{name}:")));
}

fn assert_bookmark_absent(home: &Path, repo: &Path, name: &str) {
    let mut cmd = Command::new("jj");
    cmd.current_dir(repo)
        .env("HOME", home)
        .arg("bookmark")
        .arg("list")
        .arg(name);

    cmd.assert()
        .success()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(format!(
            "No matching bookmarks for names: {name}"
        )));
}
