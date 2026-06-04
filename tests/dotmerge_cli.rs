use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[test]
fn add_accepts_home_relative_file_and_copies_into_repo_worktree() {
    let sandbox = TestSandbox::new();
    let source = sandbox.home().join(".config/dotmerge/config.toml");
    write_file(&source, "theme = \"local\"\n");

    sandbox.init_repo();

    let mut cmd = Command::cargo_bin("dotmerge").unwrap();
    cmd.env("HOME", sandbox.home())
        .arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg(".config/dotmerge/config.toml");

    cmd.assert().success();

    assert_eq!(
        fs::read_to_string(sandbox.repo().join(".config/dotmerge/config.toml")).unwrap(),
        "theme = \"local\"\n"
    );
}

#[test]
fn add_rejects_path_outside_home() {
    let sandbox = TestSandbox::new();
    let outside_path = sandbox.root().join("outside.txt");
    write_file(&outside_path, "outside\n");

    sandbox.init_repo();

    let mut cmd = Command::cargo_bin("dotmerge").unwrap();
    cmd.env("HOME", sandbox.home())
        .arg("add")
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

    let mut cmd = Command::cargo_bin("dotmerge").unwrap();
    cmd.env("HOME", sandbox.home())
        .arg("status")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());

    cmd.assert().success().stdout(
        predicate::str::contains("import: current-import = missing")
            .and(predicate::str::contains(
                "`last-sync` is missing; sync will use the empty tree as base.",
            ))
            .and(predicate::str::contains(
                "sync would leave the repo and `$HOME` unchanged",
            ))
            .and(predicate::str::contains("repo working copy is not clean").not()),
    );
}

#[test]
fn status_reports_target_already_applied_without_target_diff_details() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.home().join("foo"), "hello\n");

    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "add foo"]);

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .arg("sync")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let mut status = Command::cargo_bin("dotmerge").unwrap();
    status
        .env("HOME", sandbox.home())
        .arg("status")
        .arg("--target")
        .arg("root()")
        .arg("--repo")
        .arg(sandbox.repo());

    status.assert().success().stdout(
        predicate::str::contains("target: already applied")
            .and(predicate::str::contains("target changes since base").not())
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
    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();
    sandbox.run_jj(&["desc", "-m", "add foo"]);

    let mut initial_sync = Command::cargo_bin("dotmerge").unwrap();
    initial_sync
        .env("HOME", sandbox.home())
        .arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    initial_sync.assert().success();

    write_file(&sandbox.home().join("foo"), "local-2\n");

    let mut prepare_sync = Command::cargo_bin("dotmerge").unwrap();
    prepare_sync
        .env("HOME", sandbox.home())
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

    let mut status = Command::cargo_bin("dotmerge").unwrap();
    status
        .env("HOME", sandbox.home())
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

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .arg("sync")
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

    let mut initial_sync = Command::cargo_bin("dotmerge").unwrap();
    initial_sync
        .env("HOME", sandbox.home())
        .arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    initial_sync.assert().success();

    write_file(&sandbox.home().join("managed/foo"), "home-2\n");

    let mut prepare_sync = Command::cargo_bin("dotmerge").unwrap();
    prepare_sync
        .env("HOME", sandbox.home())
        .arg("sync")
        .arg("--no-export")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    prepare_sync.assert().success();

    let managed_dir = sandbox.home().join("managed");
    let original_mode = fs::metadata(&managed_dir).unwrap().permissions().mode();
    let readonly_mode = original_mode & !0o222;
    fs::set_permissions(&managed_dir, fs::Permissions::from_mode(readonly_mode)).unwrap();

    let mut interrupted_sync = Command::cargo_bin("dotmerge").unwrap();
    interrupted_sync
        .env("HOME", sandbox.home())
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

    let mut status = Command::cargo_bin("dotmerge").unwrap();
    status
        .env("HOME", sandbox.home())
        .arg("status")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    status.assert().success().stdout(
        predicate::str::contains("`current-import`")
            .and(predicate::str::contains("will be refreshed on sync").or(predicate::str::contains(
                "before refreshing it.",
            ))),
    );

    let mut rerun_sync = Command::cargo_bin("dotmerge").unwrap();
    rerun_sync
        .env("HOME", sandbox.home())
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

    let mut cmd = Command::cargo_bin("dotmerge").unwrap();
    cmd.env("HOME", sandbox.home())
        .arg("sync")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());

    cmd.assert().success();

    assert_bookmark_present(sandbox.home(), sandbox.repo(), "last-sync");
    assert_bookmark_absent(sandbox.home(), sandbox.repo(), "current-import");
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

    let mut cmd = Command::cargo_bin("dotmerge").unwrap();
    cmd.env("HOME", sandbox.home())
        .arg("sync")
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

    let mut cmd = Command::cargo_bin("dotmerge").unwrap();
    cmd.env("HOME", sandbox.home())
        .arg("sync")
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

    let mut initial_sync = Command::cargo_bin("dotmerge").unwrap();
    initial_sync
        .env("HOME", sandbox.home())
        .arg("sync")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    initial_sync.assert().success();

    write_file(&sandbox.home().join("managed/foo"), "home-2\n");

    let mut prepare_sync = Command::cargo_bin("dotmerge").unwrap();
    prepare_sync
        .env("HOME", sandbox.home())
        .arg("sync")
        .arg("--no-export")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    prepare_sync.assert().success();

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

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .arg("sync")
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

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .arg("sync")
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

    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .arg("add")
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

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .arg("sync")
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

    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "adding foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .arg("sync")
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
    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .env("HOSTNAME", "test-host")
        .arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();
    sandbox.run_jj(&["desc", "-m", "add foo"]);

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .env("HOSTNAME", "test-host")
        .arg("sync")
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
    assert_eq!(
        description,
        "dotmerge: merge test-host changes into origin/main"
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
    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .env("HOSTNAME", "test-host")
        .arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();
    sandbox.run_jj(&["desc", "-m", "add foo"]);

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .env("HOSTNAME", "test-host")
        .arg("sync")
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
    assert_eq!(
        description,
        format!("dotmerge: merge test-host changes into {target_short}")
    );
}

#[test]
fn sync_import_description_uses_unknown_host_when_hostname_missing() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.home().join("foo"), "old\n");
    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .env_remove("HOSTNAME")
        .arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();
    sandbox.run_jj(&["desc", "-m", "add foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);

    write_file(&sandbox.home().join("foo"), "new\n");
    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .env_remove("HOSTNAME")
        .arg("sync")
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
    assert_eq!(description, "dotmerge: import changes from unknown host");
}

#[test]
fn sync_stops_managing_target_file_deleted_in_at() {
    let sandbox = TestSandbox::new();
    sandbox.init_repo();

    write_file(&sandbox.home().join("foo"), "hello\n");

    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("foo");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "add foo"]);
    sandbox.run_jj(&["bookmark", "create", "origin/main"]);

    fs::remove_file(sandbox.repo().join("foo")).unwrap();
    sandbox.run_jj(&["describe", "-m", "remove foo"]);

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .arg("sync")
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

    let mut initial_sync = Command::cargo_bin("dotmerge").unwrap();
    initial_sync
        .env("HOME", sandbox.home())
        .arg("sync")
        .arg("--no-export")
        .arg("--target")
        .arg("origin/main")
        .arg("--repo")
        .arg(sandbox.repo());
    initial_sync.assert().success();

    sandbox.run_jj(&["new", "@"]);
    write_file(&sandbox.home().join("foo"), "hello\n");

    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .arg("add")
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

    let mut rerun_sync = Command::cargo_bin("dotmerge").unwrap();
    rerun_sync
        .env("HOME", sandbox.home())
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

    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("managed/config.toml");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "add managed config"]);

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .arg("sync")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let repo_file = sandbox.repo().join("managed/config.toml");
    let original_mode = fs::metadata(&repo_file).unwrap().permissions().mode();
    let unreadable_mode = original_mode & !0o444;
    fs::set_permissions(&repo_file, fs::Permissions::from_mode(unreadable_mode)).unwrap();

    let mut status = Command::cargo_bin("dotmerge").unwrap();
    status
        .env("HOME", sandbox.home())
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

    let mut add = Command::cargo_bin("dotmerge").unwrap();
    add.env("HOME", sandbox.home())
        .arg("add")
        .arg("--repo")
        .arg(sandbox.repo())
        .arg("managed/large.bin");
    add.assert().success();

    sandbox.run_jj(&["desc", "-m", "add large tracked file"]);

    let mut sync = Command::cargo_bin("dotmerge").unwrap();
    sync.env("HOME", sandbox.home())
        .arg("sync")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());
    sync.assert().success();

    let repo_file = sandbox.repo().join("managed/large.bin");
    let mut modified_contents = fs::read(&repo_file).unwrap();
    modified_contents[0] ^= 0xff;
    fs::write(&repo_file, modified_contents).unwrap();

    let mut status = Command::cargo_bin("dotmerge").unwrap();
    status
        .env("HOME", sandbox.home())
        .arg("status")
        .arg("--target")
        .arg("@")
        .arg("--repo")
        .arg(sandbox.repo());
    status.assert().success().stdout(predicate::str::contains(
        "repo working copy is not clean",
    ));
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
        Self {
            tempdir,
            home,
            repo,
        }
    }

    fn root(&self) -> &Path {
        self.tempdir.path()
    }

    fn home(&self) -> &Path {
        &self.home
    }

    fn repo(&self) -> &Path {
        &self.repo
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
