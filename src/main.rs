// Per-repo incremental change tracker. Multiple independent consumers (cleanup
// skills run by separate agent loops) each keep their own baseline of which file
// contents they've already processed; when a file changes, every consumer that
// hasn't seen those exact bytes sees it again. State lives in the git dir, never
// committed, keyed per consumer so concurrent consumers are each the sole writer
// of their own file and no locking is needed.

mod mcp;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::hash::Hasher;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "go.sum",
    "Gemfile.lock",
    "poetry.lock",
    "Pipfile.lock",
    "composer.lock",
    "flake.lock",
];

// Git's hash of the empty tree — a valid diff base in any repo, including one
// with no commits yet. Diffing against it surfaces the whole staged/tracked set
// before the first commit, where `HEAD` doesn't resolve.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Default, Deserialize, Serialize)]
struct State {
    #[serde(default)]
    clean: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ConsumerStatus {
    consumer: String,
    tracked: usize,
    changed: usize,
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        // Runtime failures exit 2 so `list -q`'s exit 1 stays an unambiguous
        // "no changes" signal and never collides with an error.
        Err(err) => {
            eprintln!("error: {err:#}");
            2
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let mut consumer = "default".to_string();
    let mut explicit_consumer = false;
    let mut all = false;
    let mut null = false;
    let mut quiet = false;
    let mut stdin = false;
    let mut json = false;
    let mut each = false;
    let mut subcommand: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut command: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--as" => {
                consumer = args.next().context("--as requires a consumer name")?;
                explicit_consumer = true;
            }
            "--all" => all = true,
            "-z" | "--null" => null = true,
            "-q" | "--quiet" => quiet = true,
            "--stdin" => stdin = true,
            "--json" => json = true,
            "--each" => each = true,
            "-v" | "--version" => {
                println!("glean {}", env!("CARGO_PKG_VERSION"));
                return Ok(0);
            }
            // Everything past `--` is the verbatim command for `run`.
            "--" => {
                command.extend(args.by_ref());
                break;
            }
            _ if subcommand.is_none() => subcommand = Some(arg),
            _ => paths.push(arg),
        }
    }

    match subcommand.as_deref() {
        Some("list") => list(&consumer, null, quiet, json),
        Some("mark") => mark(&consumer, &paths, stdin, null),
        Some("status") => status(&consumer, explicit_consumer, json),
        Some("reset") => reset(&consumer, all),
        Some("run") => run_cmd(&consumer, each, &command),
        Some("mcp") => mcp::serve(),
        _ => Ok(usage()),
    }
}

fn usage() -> i32 {
    eprintln!(
        "usage: glean <list|mark|status|reset|run|mcp> [--as <consumer>] [options] [paths...]\n\
         \n\
         list    [--as <name>] [-z|--null] [-q|--quiet] [--json]\n\
         mark    [--as <name>] [--stdin] [-z|--null] [paths...]\n\
         status  [--as <name>] [--json]\n\
         reset   [--as <name>] [--all]\n\
         run     [--as <name>] [--each] -- <cmd> [args...]\n\
         mcp\n\
         \n\
         -v, --version"
    );
    2
}

fn list(consumer: &str, null: bool, quiet: bool, json: bool) -> Result<i32> {
    if null && json {
        eprintln!("error: --null and --json are mutually exclusive");
        return Ok(2);
    }
    let repo = Repo::discover(consumer)?;
    let changed = repo.changed()?;

    if quiet {
        return Ok(if changed.is_empty() { 1 } else { 0 });
    }
    if json {
        println!("{}", serde_json::to_string(&changed)?);
    } else if null {
        let mut out = std::io::stdout().lock();
        for path in &changed {
            out.write_all(path.as_bytes())?;
            out.write_all(b"\0")?;
        }
    } else {
        for path in &changed {
            println!("{path}");
        }
    }
    Ok(0)
}

fn mark(consumer: &str, paths: &[String], stdin: bool, null: bool) -> Result<i32> {
    let repo = Repo::discover(consumer)?;
    let marked = if stdin {
        let paths = read_stdin_paths(null)?;
        // A piped selection of nothing marks nothing; only a bare `mark` with no
        // selection at all falls through to "the whole changed set".
        if paths.is_empty() {
            0
        } else {
            repo.mark_paths(&paths)?
        }
    } else {
        repo.mark_paths(paths)?
    };
    eprintln!("marked {marked} files");
    Ok(0)
}

fn status(consumer: &str, explicit_consumer: bool, json: bool) -> Result<i32> {
    let repo = Repo::discover(consumer)?;
    let names = if explicit_consumer {
        vec![consumer.to_string()]
    } else {
        let consumers = repo.consumers()?;
        if consumers.is_empty() {
            if json {
                println!("[]");
            } else {
                println!("no glean state for any consumer");
            }
            return Ok(0);
        }
        consumers
    };

    let statuses = repo.status_for(&names)?;
    if json {
        println!("{}", serde_json::to_string(&statuses)?);
    } else {
        for s in &statuses {
            println!(
                "{}: {} tracked, {} changed",
                s.consumer, s.tracked, s.changed
            );
        }
    }
    Ok(0)
}

fn reset(consumer: &str, all: bool) -> Result<i32> {
    let repo = Repo::discover(consumer)?;
    if all {
        // Drop the whole dir, not just the files, so nothing lingers in .git.
        let dir = repo.git_dir.join("glean");
        if std::fs::remove_dir_all(&dir).is_ok() {
            eprintln!("removed {}", dir.display());
        }
        return Ok(0);
    }
    let path = repo.state_path(consumer);
    if std::fs::remove_file(&path).is_ok() {
        eprintln!("removed {}", path.display());
    }
    Ok(0)
}

// Snapshot the changed set once, run the command on it, mark on success. One
// process means no window between deciding the work and recording it done.
fn run_cmd(consumer: &str, each: bool, command: &[String]) -> Result<i32> {
    if command.is_empty() {
        return Ok(usage());
    }
    let repo = Repo::discover(consumer)?;
    let files = repo.changed()?;
    if files.is_empty() {
        return Ok(0);
    }

    if each {
        // Per-file so a clean file settles while a failing one keeps coming back,
        // instead of one bad file pinning the whole batch as unprocessed.
        let mut ok = Vec::new();
        for file in &files {
            if spawn(command, std::slice::from_ref(file))?.success() {
                ok.push(file.clone());
            }
        }
        let marked = if ok.is_empty() {
            0
        } else {
            repo.mark_paths(&ok)?
        };
        eprintln!("marked {marked} files");
        Ok(if ok.len() == files.len() { 0 } else { 1 })
    } else {
        let status = spawn(command, &files)?;
        if status.success() {
            let marked = repo.mark_paths(&files)?;
            eprintln!("marked {marked} files");
        }
        // A signal-killed child has no code; treat it as a generic failure.
        Ok(status.code().unwrap_or(1))
    }
}

fn spawn(command: &[String], files: &[String]) -> Result<std::process::ExitStatus> {
    Command::new(&command[0])
        .args(&command[1..])
        .args(files)
        .status()
        .with_context(|| format!("running {}", command[0]))
}

fn read_stdin_paths(null: bool) -> Result<Vec<String>> {
    let mut buf = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buf)
        .context("reading stdin")?;
    let text = String::from_utf8_lossy(&buf);
    let sep = if null { '\0' } else { '\n' };
    Ok(text
        .split(sep)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

struct Repo {
    toplevel: PathBuf,
    git_dir: PathBuf,
    consumer: String,
}

impl Repo {
    fn discover(consumer: &str) -> Result<Self> {
        let toplevel =
            git_line(&["rev-parse", "--show-toplevel"]).context("not inside a git repository")?;
        let git_dir = git_line(&["rev-parse", "--absolute-git-dir"])
            .context("not inside a git repository")?;
        Ok(Self {
            toplevel: PathBuf::from(toplevel),
            git_dir: PathBuf::from(git_dir),
            consumer: consumer.to_string(),
        })
    }

    fn state_path(&self, consumer: &str) -> PathBuf {
        self.git_dir.join("glean").join(format!("{consumer}.json"))
    }

    // Content hash of every trackable candidate, keyed by repo-relative path.
    // BTreeMap iteration is sorted, so the change list comes out in path order.
    fn current_hashes(&self) -> Result<BTreeMap<String, String>> {
        let mut hashes = BTreeMap::new();
        for path in self.candidates()? {
            if let Some(hash) = self.hash_file(&path) {
                hashes.insert(path, hash);
            }
        }
        Ok(hashes)
    }

    fn changed_paths(current: &BTreeMap<String, String>, state: &State) -> Vec<String> {
        current
            .iter()
            .filter(|(path, hash)| state.clean.get(*path) != Some(*hash))
            .map(|(path, _)| path.clone())
            .collect()
    }

    // Sorted paths whose current contents differ from this consumer's baseline.
    fn changed(&self) -> Result<Vec<String>> {
        let current = self.current_hashes()?;
        Ok(Self::changed_paths(&current, &self.load_state()))
    }

    // Records paths as processed and returns how many baselines moved. Empty
    // `paths` marks the whole current candidate set.
    fn mark_paths(&self, paths: &[String]) -> Result<usize> {
        let mut state = self.load_state();
        let candidates: HashSet<String> = self.candidates()?.into_iter().collect();

        let mut marked = 0usize;
        let mut just_marked = HashSet::new();
        if paths.is_empty() {
            // Mark from the change-detection pass directly so each file is read once.
            for path in &candidates {
                if let Some(hash) = self.hash_file(path) {
                    just_marked.insert(path.clone());
                    if state.clean.get(path) != Some(&hash) {
                        state.clean.insert(path.clone(), hash);
                        marked += 1;
                    }
                }
            }
        } else {
            for path in paths {
                match self.hash_file(path) {
                    Some(hash) => {
                        state.clean.insert(path.clone(), hash);
                        just_marked.insert(path.clone());
                        marked += 1;
                    }
                    None => {
                        state.clean.remove(path);
                    }
                }
            }
        }

        // An entry only affects `list` when its path is a current candidate, so
        // dropping non-candidates can't change what `list` reports; the worst case
        // is a re-marked-identical file getting one extra no-op pass. Keeping
        // just-marked paths covers selective marks of files git doesn't surface.
        state
            .clean
            .retain(|path, _| candidates.contains(path) || just_marked.contains(path));

        self.save_state(&state)?;
        Ok(marked)
    }

    fn status_for(&self, names: &[String]) -> Result<Vec<ConsumerStatus>> {
        let current = self.current_hashes()?;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let state = self.load_state_for(name);
            out.push(ConsumerStatus {
                consumer: name.clone(),
                tracked: state.clean.len(),
                changed: Self::changed_paths(&current, &state).len(),
            });
        }
        Ok(out)
    }

    fn candidates(&self) -> Result<Vec<String>> {
        let mut paths = BTreeSet::new();

        // Before the first commit `HEAD` doesn't resolve, so a staged file
        // wouldn't show up against it (and it's no longer untracked either).
        // Diff against the empty tree there to surface the staged set instead.
        let base = if self.has_head() { "HEAD" } else { EMPTY_TREE };
        paths.extend(git_z(&self.toplevel, &["diff", base, "-z", "--name-only"])?);
        paths.extend(git_z(
            &self.toplevel,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?);

        Ok(paths.into_iter().collect())
    }

    fn has_head(&self) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(&self.toplevel)
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .output()
            .is_ok_and(|out| out.status.success())
    }

    // Lockfiles and binary files return None, dropping them from the changed
    // set so a consumer only sees reviewable text changes. The file is streamed
    // so a multi-GB blob never lands in memory: a NUL in the first chunk rejects
    // it before reading the rest, and otherwise the hash is fed chunk by chunk.
    fn hash_file(&self, repo_rel: &str) -> Option<String> {
        let basename = Path::new(repo_rel).file_name()?.to_str()?;
        if LOCKFILES.contains(&basename) {
            return None;
        }

        let mut file = std::fs::File::open(self.toplevel.join(repo_rel)).ok()?;
        let mut prefix = vec![0u8; 8192];
        let mut filled = 0;
        while filled < prefix.len() {
            let n = file.read(&mut prefix[filled..]).ok()?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        let prefix = &prefix[..filled];
        if prefix.contains(&0) {
            return None;
        }

        let mut hasher = DefaultHasher::new();
        hasher.write(prefix);
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            hasher.write(&buf[..n]);
        }
        Some(format!("{:016x}", hasher.finish()))
    }

    fn load_state(&self) -> State {
        self.load_state_for(&self.consumer)
    }

    fn load_state_for(&self, consumer: &str) -> State {
        std::fs::read(self.state_path(consumer))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save_state(&self, state: &State) -> Result<()> {
        let target = self.state_path(&self.consumer);
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let json = serde_json::to_string_pretty(state)?;
        // Write to a sibling temp file then rename over the target so a crash
        // mid-write can't leave a torn state file.
        let tmp = target.with_extension("json.tmp");
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &target).with_context(|| format!("replacing {}", target.display()))
    }

    fn consumers(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.git_dir.join("glean")) else {
            return Ok(names);
        };
        for entry in entries {
            let name = entry?.file_name();
            let name = name.to_string_lossy();
            // A concurrent save's <consumer>.json.tmp ends in .tmp, not .json.
            if let Some(consumer) = name.strip_suffix(".json") {
                names.push(consumer.to_string());
            }
        }
        names.sort();
        Ok(names)
    }
}

fn git_line(args: &[&str]) -> Result<String> {
    let out = git(None, args)?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

fn git_z(dir: &Path, args: &[&str]) -> Result<Vec<String>> {
    let out = git(Some(dir), args)?;
    Ok(String::from_utf8_lossy(&out)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

fn git(dir: Option<&Path>, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    let out = cmd.args(args).output().context("failed to run git")?;
    if !out.status.success() {
        bail!("git {} failed", args.join(" "));
    }
    Ok(out.stdout)
}
