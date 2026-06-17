// Integration tests drive the built `glean` binary against throwaway git repos
// in temp dirs, exercising the CLI surface and the per-consumer state contract
// rather than any internal function.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicU32, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_glean");

// Removes the repo's temp dir on drop so a panicking test still cleans up.
struct Repo {
    dir: PathBuf,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Repo {
    fn path(&self) -> &Path {
        &self.dir
    }
}

fn unique_dir(test_name: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("glean-test-{test_name}-{pid}-{n}"))
}

fn git<I, S>(dir: &Path, args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("failed to run git");
    assert!(
        out.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_line(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("failed to run git");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn glean(dir: &Path, args: &[&str]) -> (String, ExitStatus) {
    let out = Command::new(BIN)
        .current_dir(dir)
        .args(args)
        .output()
        .expect("failed to run glean");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status,
    )
}

// stdout split into trimmed non-empty lines, for asserting on `list`/`status`.
fn lines(stdout: &str) -> Vec<String> {
    stdout.lines().map(str::trim).map(str::to_string).collect()
}

fn write(dir: &Path, rel: &str, contents: &[u8]) {
    fs::write(dir.join(rel), contents).expect("write file");
}

// A git repo with one committed text file, ready for glean to run against.
fn mk_repo(test_name: &str) -> Repo {
    let dir = unique_dir(test_name);
    fs::create_dir_all(&dir).expect("create temp dir");
    git(&dir, ["init", "-q"]);
    git(&dir, ["config", "user.email", "test@example.com"]);
    git(&dir, ["config", "user.name", "Test"]);
    write(&dir, "tracked.txt", b"hello\n");
    git(&dir, ["add", "tracked.txt"]);
    git(&dir, ["commit", "-q", "-m", "init"]);
    Repo { dir }
}

#[test]
fn changed_then_clean() {
    let repo = mk_repo("changed_then_clean");
    let dir = repo.path();

    write(dir, "tracked.txt", b"edited\n");
    let (out, status) = glean(dir, &["list"]);
    assert!(status.success());
    assert_eq!(lines(&out), vec!["tracked.txt"]);

    let (_, status) = glean(dir, &["mark"]);
    assert!(status.success());

    let (out, status) = glean(dir, &["list"]);
    assert!(status.success());
    assert!(lines(&out).is_empty(), "list should be empty, got {out:?}");
}

#[test]
fn per_consumer_independence() {
    let repo = mk_repo("per_consumer_independence");
    let dir = repo.path();

    write(dir, "tracked.txt", b"edited\n");
    assert_eq!(
        lines(&glean(dir, &["list", "--as", "a"]).0),
        vec!["tracked.txt"]
    );
    assert_eq!(
        lines(&glean(dir, &["list", "--as", "b"]).0),
        vec!["tracked.txt"]
    );

    glean(dir, &["mark", "--as", "a"]);

    assert!(
        lines(&glean(dir, &["list", "--as", "a"]).0).is_empty(),
        "a should be clean after its own mark"
    );
    assert_eq!(
        lines(&glean(dir, &["list", "--as", "b"]).0),
        vec!["tracked.txt"],
        "b's baseline is untouched by a's mark"
    );
}

#[test]
fn re_change_detection() {
    let repo = mk_repo("re_change_detection");
    let dir = repo.path();

    write(dir, "tracked.txt", b"edited\n");
    glean(dir, &["mark"]);
    assert!(lines(&glean(dir, &["list"]).0).is_empty());

    write(dir, "tracked.txt", b"edited again\n");
    assert_eq!(lines(&glean(dir, &["list"]).0), vec!["tracked.txt"]);
}

#[test]
fn selective_mark() {
    let repo = mk_repo("selective_mark");
    let dir = repo.path();

    write(dir, "file1.txt", b"one\n");
    write(dir, "file2.txt", b"two\n");
    assert_eq!(
        lines(&glean(dir, &["list", "--as", "x"]).0),
        vec!["file1.txt", "file2.txt"]
    );

    glean(dir, &["mark", "--as", "x", "file1.txt"]);

    assert_eq!(
        lines(&glean(dir, &["list", "--as", "x"]).0),
        vec!["file2.txt"],
        "only the named file is marked clean"
    );
}

#[test]
fn noise_skip() {
    let repo = mk_repo("noise_skip");
    let dir = repo.path();

    write(dir, "Cargo.lock", b"lockfile contents\n");
    write(dir, "binary.bin", b"before\0after");
    write(dir, "real.txt", b"reviewable\n");

    let listed = lines(&glean(dir, &["list"]).0);
    assert_eq!(
        listed,
        vec!["real.txt"],
        "lockfile and NUL file are skipped"
    );
}

#[test]
fn nul_in_prefix_skips_streaming() {
    let repo = mk_repo("nul_in_prefix_skips_streaming");
    let dir = repo.path();

    // Text for the first 4 KiB, a NUL, then more text — all inside the 8 KiB
    // sniff window, so the streaming hasher rejects it before reading further.
    let mut sniffed_binary = vec![b'a'; 4096];
    sniffed_binary.push(0);
    sniffed_binary.extend_from_slice(&[b'b'; 4096]);
    write(dir, "sniffed.bin", &sniffed_binary);
    write(dir, "real.txt", b"reviewable\n");

    assert_eq!(
        lines(&glean(dir, &["list"]).0),
        vec!["real.txt"],
        "NUL within the prefix window skips the file"
    );
}

#[test]
fn mark_prunes_non_candidate_entries() {
    let repo = mk_repo("mark_prunes_non_candidate_entries");
    let dir = repo.path();

    // Mark untracked A clean, then commit it so it leaves the candidate set.
    write(dir, "a.txt", b"alpha\n");
    glean(dir, &["mark", "--as", "c", "a.txt"]);
    git(dir, ["add", "a.txt"]);
    git(dir, ["commit", "-q", "-m", "add a"]);

    // A different mark must prune A's now-stale entry.
    write(dir, "tracked.txt", b"edited\n");
    glean(dir, &["mark", "--as", "c", "tracked.txt"]);

    let git_dir = PathBuf::from(git_line(dir, &["rev-parse", "--absolute-git-dir"]));
    let state = fs::read_to_string(git_dir.join("glean").join("c.json")).expect("read state");
    assert!(
        !state.contains("a.txt"),
        "A is no longer a candidate, so its entry is pruned: {state}"
    );
    assert!(
        state.contains("tracked.txt"),
        "the just-marked candidate stays: {state}"
    );
}

#[test]
fn state_location() {
    let repo = mk_repo("state_location");
    let dir = repo.path();

    write(dir, "tracked.txt", b"edited\n");
    glean(dir, &["mark", "--as", "consumer1"]);

    let git_dir = PathBuf::from(git_line(dir, &["rev-parse", "--absolute-git-dir"]));
    let state = git_dir.join("glean").join("consumer1.json");
    assert!(state.is_file(), "state file missing at {}", state.display());
    assert_eq!(state.parent().unwrap(), git_dir.join("glean"));
}

#[test]
fn status_and_reset() {
    let repo = mk_repo("status_and_reset");
    let dir = repo.path();

    write(dir, "tracked.txt", b"edited\n");
    glean(dir, &["mark", "--as", "a"]);
    glean(dir, &["mark", "--as", "b"]);

    let status = lines(&glean(dir, &["status"]).0);
    assert_eq!(status.len(), 2, "one line per consumer, got {status:?}");
    assert!(status[0].starts_with("a:"));
    assert!(status[1].starts_with("b:"));

    glean(dir, &["reset", "--as", "a"]);
    let status = lines(&glean(dir, &["status"]).0);
    assert_eq!(
        status.len(),
        1,
        "reset --as a removes only a, got {status:?}"
    );
    assert!(status[0].starts_with("b:"));

    glean(dir, &["reset", "--all"]);
    let status = lines(&glean(dir, &["status"]).0);
    assert_eq!(status, vec!["no glean state for any consumer"]);
    assert!(
        !dir.join(".git/glean").exists(),
        "reset --all removes the whole .git/glean dir"
    );
}

#[test]
fn no_commit_repo() {
    let dir = unique_dir("no_commit_repo");
    fs::create_dir_all(&dir).expect("create temp dir");
    let repo = Repo { dir };
    let dir = repo.path();

    git(dir, ["init", "-q"]);
    git(dir, ["config", "user.email", "test@example.com"]);
    git(dir, ["config", "user.name", "Test"]);
    write(dir, "untracked.txt", b"new\n");

    let (out, status) = glean(dir, &["list"]);
    assert!(status.success(), "list must not fail without HEAD");
    assert_eq!(lines(&out), vec!["untracked.txt"]);
}

#[test]
fn no_commit_staged_file() {
    let dir = unique_dir("no_commit_staged_file");
    fs::create_dir_all(&dir).expect("create temp dir");
    let repo = Repo { dir };
    let dir = repo.path();

    git(dir, ["init", "-q"]);
    git(dir, ["config", "user.email", "test@example.com"]);
    git(dir, ["config", "user.name", "Test"]);

    // Staged, then modified again — no longer untracked, and HEAD doesn't exist.
    write(dir, "staged.txt", b"staged\n");
    git(dir, ["add", "staged.txt"]);
    write(dir, "staged.txt", b"staged\nmodified\n");

    // Plain staged-but-unmodified — a candidate too, nothing's marked yet.
    write(dir, "clean.txt", b"clean\n");
    git(dir, ["add", "clean.txt"]);

    let (out, status) = glean(dir, &["list"]);
    assert!(status.success(), "list must not fail without HEAD");
    assert_eq!(lines(&out), vec!["clean.txt", "staged.txt"]);
}

#[test]
fn usage_exit_code() {
    let repo = mk_repo("usage_exit_code");
    let dir = repo.path();

    let (_, status) = glean(dir, &["bogus"]);
    assert_eq!(status.code(), Some(2), "unknown subcommand exits 2");

    let (_, status) = glean(dir, &[]);
    assert_eq!(status.code(), Some(2), "missing subcommand exits 2");
}
