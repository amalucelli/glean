// Per-repo incremental change tracker. Multiple independent consumers (cleanup
// skills run by separate agent loops) each keep their own baseline of which file
// contents they've already processed; when a file changes, every consumer that
// hasn't seen those exact bytes sees it again. State lives in the git dir, never
// committed, keyed per consumer so concurrent consumers are each the sole writer
// of their own file and no locking is needed.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::hash::Hasher;
use std::io::Read;
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

fn main() -> Result<()> {
    let mut consumer = "default".to_string();
    let mut explicit_consumer = false;
    let mut all = false;
    let mut subcommand: Option<String> = None;
    let mut paths: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--as" => {
                consumer = args.next().context("--as requires a consumer name")?;
                explicit_consumer = true;
            }
            "--all" => all = true,
            _ if subcommand.is_none() => subcommand = Some(arg),
            _ => paths.push(arg),
        }
    }

    match subcommand.as_deref() {
        Some("list") => list(&consumer),
        Some("mark") => mark(&consumer, &paths),
        Some("status") => status(&consumer, explicit_consumer),
        Some("reset") => reset(&consumer, all),
        _ => {
            eprintln!("usage: glean <list|mark|status|reset> [--as <consumer>] [--all] [paths...]");
            std::process::exit(2);
        }
    }
}

fn list(consumer: &str) -> Result<()> {
    let repo = Repo::discover(consumer)?;
    let state = repo.load_state();
    let current = repo.current_hashes()?;
    for path in Repo::changed_paths(&current, &state) {
        println!("{path}");
    }
    Ok(())
}

fn mark(consumer: &str, paths: &[String]) -> Result<()> {
    let repo = Repo::discover(consumer)?;
    let mut state = repo.load_state();

    let candidates: HashSet<String> = repo.candidates()?.into_iter().collect();

    let mut marked = 0usize;
    let mut just_marked = HashSet::new();
    if paths.is_empty() {
        // Mark from the change-detection pass directly so each file is read once.
        for path in &candidates {
            if let Some(hash) = repo.hash_file(path) {
                just_marked.insert(path.clone());
                if state.clean.get(path) != Some(&hash) {
                    state.clean.insert(path.clone(), hash);
                    marked += 1;
                }
            }
        }
    } else {
        for path in paths {
            match repo.hash_file(path) {
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

    repo.save_state(&state)?;
    eprintln!("marked {marked} files");
    Ok(())
}

fn status(consumer: &str, explicit_consumer: bool) -> Result<()> {
    let repo = Repo::discover(consumer)?;
    let current = repo.current_hashes()?;

    let names = if explicit_consumer {
        vec![consumer.to_string()]
    } else {
        let consumers = repo.consumers()?;
        if consumers.is_empty() {
            println!("no glean state for any consumer");
            return Ok(());
        }
        consumers
    };

    for name in names {
        let state = repo.load_state_for(&name);
        let changed = Repo::changed_paths(&current, &state).len();
        println!("{name}: {} tracked, {changed} changed", state.clean.len());
    }
    Ok(())
}

fn reset(consumer: &str, all: bool) -> Result<()> {
    let repo = Repo::discover(consumer)?;
    if all {
        // Drop the whole dir, not just the files, so nothing lingers in .git.
        let dir = repo.git_dir.join("glean");
        if std::fs::remove_dir_all(&dir).is_ok() {
            eprintln!("removed {}", dir.display());
        }
        return Ok(());
    }
    let path = repo.state_path(consumer);
    if std::fs::remove_file(&path).is_ok() {
        eprintln!("removed {}", path.display());
    }
    Ok(())
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
