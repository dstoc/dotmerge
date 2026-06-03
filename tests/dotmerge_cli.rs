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
