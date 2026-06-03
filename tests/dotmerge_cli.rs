use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
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
        last_sync_description, "dotmerge import from home",
        "expected sync to reuse the prepared import instead of creating a merge commit"
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

    let current_import = sandbox
        .jj_stdout(&[
            "log",
            "-r",
            "current-import",
            "--no-graph",
            "-T",
            "commit_id",
        ])
        .trim()
        .to_owned();
    assert!(
        !current_import.is_empty(),
        "expected `current-import` to exist after `sync --no-export`"
    );

    let ancestry = sandbox.jj_stdout(&[
        "log",
        "-r",
        &format!("{repo_side_before_rerun}::current-import"),
        "--no-graph",
        "-T",
        "commit_id ++ \"\\n\"",
    ]);
    assert!(
        ancestry
            .lines()
            .any(|line| line.trim() == repo_side_before_rerun),
        "expected rerun sync to recreate `current-import` after the repo-side state; ancestry was:\n{ancestry}"
    );
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
