use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn git(directory: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(directory)
        .args(args)
        .output()
        .expect("Git should be installed for integration tests")
}

fn git_ok(directory: &Path, args: &[&str]) -> String {
    let output = git(directory, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output should be UTF-8")
        .trim()
        .to_owned()
}

fn init_repository(object_format: Option<&str>) -> Option<TempDir> {
    let directory = TempDir::new().expect("temporary repository");
    let mut args = vec!["init", "-q", "-b", "main"];
    let format_argument;
    if let Some(format) = object_format {
        format_argument = format!("--object-format={format}");
        args.push(&format_argument);
    }
    let output = git(directory.path(), &args);
    if !output.status.success() {
        return None;
    }

    git_ok(directory.path(), &["config", "user.name", "Shasha Test"]);
    git_ok(
        directory.path(),
        &["config", "user.email", "shasha@example.invalid"],
    );
    Some(directory)
}

fn run_shasha(directory: &Path, prefix_len: u8, message: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_shasha"))
        .current_dir(directory)
        .args([
            "--length",
            &prefix_len.to_string(),
            "--threads",
            "2",
            "-m",
            message,
        ])
        .output()
        .expect("shasha should run")
}

#[test]
fn creates_self_identifying_sha1_commits() {
    let repository = init_repository(None).expect("SHA-1 repositories should be supported");
    fs::write(repository.path().join("app.txt"), "first\n").unwrap();
    fs::write(repository.path().join(".gitignore"), ".shasha\n").unwrap();
    git_ok(repository.path(), &["add", "app.txt", ".gitignore"]);

    let first_run = run_shasha(repository.path(), 4, "initial commit");
    assert!(
        first_run.status.success(),
        "{}",
        String::from_utf8_lossy(&first_run.stderr)
    );
    let first_oid = git_ok(repository.path(), &["rev-parse", "HEAD"]);
    let first_prefix = fs::read_to_string(repository.path().join(".shasha"))
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(first_prefix.len(), 4);
    assert!(first_oid.starts_with(&first_prefix));
    assert_eq!(
        git_ok(repository.path(), &["show", "HEAD:.shasha"]),
        first_prefix
    );
    assert!(git_ok(repository.path(), &["cat-file", "-p", "HEAD"]).contains("Shasha-Nonce: "));
    assert_eq!(git_ok(repository.path(), &["status", "--porcelain"]), "");

    fs::write(repository.path().join("app.txt"), "second\n").unwrap();
    git_ok(repository.path(), &["add", "app.txt"]);
    let second_run = run_shasha(repository.path(), 4, "second commit");
    assert!(
        second_run.status.success(),
        "{}",
        String::from_utf8_lossy(&second_run.stderr)
    );
    let second_oid = git_ok(repository.path(), &["rev-parse", "HEAD"]);
    let second_prefix = fs::read_to_string(repository.path().join(".shasha"))
        .unwrap()
        .trim()
        .to_owned();
    assert!(second_oid.starts_with(&second_prefix));
    assert_eq!(
        git_ok(repository.path(), &["rev-parse", "HEAD^"]),
        first_oid
    );
    assert_eq!(git_ok(repository.path(), &["status", "--porcelain"]), "");
    git_ok(repository.path(), &["fsck", "--strict"]);
}

#[test]
fn creates_a_self_identifying_sha256_commit_when_git_supports_it() {
    let Some(repository) = init_repository(Some("sha256")) else {
        eprintln!("Git does not support SHA-256 repositories; skipping");
        return;
    };
    fs::write(repository.path().join("app.txt"), "sha256\n").unwrap();
    git_ok(repository.path(), &["add", "app.txt"]);

    let run = run_shasha(repository.path(), 4, "SHA-256 commit");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let oid = git_ok(repository.path(), &["rev-parse", "HEAD"]);
    let prefix = fs::read_to_string(repository.path().join(".shasha"))
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(oid.len(), 64);
    assert_eq!(prefix.len(), 4);
    assert!(oid.starts_with(&prefix));
    git_ok(repository.path(), &["fsck", "--strict"]);
}

#[test]
fn continues_a_branch_with_packed_refs_and_objects() {
    let repository = init_repository(None).expect("SHA-1 repositories should be supported");
    fs::write(repository.path().join("app.txt"), "first\n").unwrap();
    git_ok(repository.path(), &["add", "app.txt"]);
    let first = run_shasha(repository.path(), 2, "before packing refs");
    assert!(first.status.success());
    let first_oid = git_ok(repository.path(), &["rev-parse", "HEAD"]);

    git_ok(repository.path(), &["gc", "--prune=now"]);
    assert!(!repository.path().join(".git/refs/heads/main").exists());
    assert!(
        fs::read_dir(repository.path().join(".git/objects/pack"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|extension| extension == "idx"))
    );
    fs::write(repository.path().join("app.txt"), "second\n").unwrap();
    git_ok(repository.path(), &["add", "app.txt"]);

    let second = run_shasha(repository.path(), 2, "after packing refs");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        git_ok(repository.path(), &["rev-parse", "HEAD^"]),
        first_oid
    );
    git_ok(repository.path(), &["fsck", "--strict"]);
}

#[test]
fn works_from_a_nested_directory_in_a_linked_worktree() {
    let repository = init_repository(None).expect("SHA-1 repositories should be supported");
    fs::write(repository.path().join("app.txt"), "base\n").unwrap();
    git_ok(repository.path(), &["add", "app.txt"]);
    git_ok(repository.path(), &["commit", "-q", "-m", "base"]);

    let worktree_container = TempDir::new().unwrap();
    let worktree = worktree_container.path().join("checkout");
    git_ok(
        repository.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().unwrap(),
        ],
    );
    fs::write(worktree.join("app.txt"), "worktree\n").unwrap();
    git_ok(&worktree, &["add", "app.txt"]);
    let nested = worktree.join("deep/nested");
    fs::create_dir_all(&nested).unwrap();

    let run = run_shasha(&nested, 2, "linked worktree commit");
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let oid = git_ok(&worktree, &["rev-parse", "HEAD"]);
    let prefix = fs::read_to_string(worktree.join(".shasha")).unwrap();
    assert!(oid.starts_with(prefix.trim()));
    assert_eq!(git_ok(&worktree, &["branch", "--show-current"]), "feature");
    git_ok(repository.path(), &["fsck", "--strict"]);
}

#[test]
fn refuses_to_commit_during_an_in_progress_merge() {
    let repository = init_repository(None).expect("SHA-1 repositories should be supported");
    fs::write(repository.path().join("app.txt"), "content\n").unwrap();
    git_ok(repository.path(), &["add", "app.txt"]);
    let git_dir = git_ok(repository.path(), &["rev-parse", "--git-dir"]);
    fs::write(
        repository.path().join(git_dir).join("MERGE_HEAD"),
        "deadbeef\n",
    )
    .unwrap();

    let run = run_shasha(repository.path(), 1, "should fail");
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains("MERGE_HEAD"));
    assert!(!repository.path().join(".shasha").exists());
}

#[cfg(unix)]
#[test]
fn refuses_to_overwrite_a_symbolic_link_as_the_version_file() {
    use std::os::unix::fs::symlink;

    let repository = init_repository(None).expect("SHA-1 repositories should be supported");
    let target = repository.path().join("do-not-touch");
    fs::write(&target, "original\n").unwrap();
    symlink("do-not-touch", repository.path().join(".shasha")).unwrap();

    let run = run_shasha(repository.path(), 1, "should fail");
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains("symbolic link"));
    assert_eq!(fs::read_to_string(target).unwrap(), "original\n");
}
