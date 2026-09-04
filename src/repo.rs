// The repository, its consumers, and the baselines that make "changed" mean
// something different to each of them.
//
// A baseline is a map from repo-relative path to a content hash. State lives in
// the git dir under one file per consumer, which is what makes concurrent
// consumers safe without locking: each is the sole writer of its own file, and
// a save goes through a temp file and a rename so a crash cannot leave a torn
// one behind.
//
// git supplies the candidates and nothing else. What counts as changed is
// decided by hashing, so a file touched and reverted settles instead of staying
// dirty forever.

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

#[derive(Serialize)]
pub struct ConsumerStatus {
    pub consumer: String,
    pub tracked: usize,
    pub changed: usize,
    // Unix seconds; None when the consumer has no state file yet.
    pub last_marked: Option<u64>,
}

pub struct Repo {
    toplevel: PathBuf,
    pub git_dir: PathBuf,
    consumer: String,
}

impl Repo {
    pub fn discover(consumer: &str) -> Result<Self> {
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

    pub fn state_path(&self, consumer: &str) -> PathBuf {
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
    pub fn changed(&self) -> Result<Vec<String>> {
        let current = self.current_hashes()?;
        Ok(Self::changed_paths(&current, &self.load_state()))
    }

    // Records paths as processed and returns how many baselines moved. Empty
    // `paths` marks the whole current candidate set.
    pub fn mark_paths(&self, paths: &[String]) -> Result<usize> {
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

    pub fn status_for(&self, names: &[String]) -> Result<Vec<ConsumerStatus>> {
        let current = self.current_hashes()?;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let state = self.load_state_for(name);
            out.push(ConsumerStatus {
                consumer: name.clone(),
                tracked: state.clean.len(),
                changed: Self::changed_paths(&current, &state).len(),
                last_marked: self.last_marked(name),
            });
        }
        Ok(out)
    }

    // `mark` is the only writer of a state file, so its mtime is when this
    // consumer last marked — no timestamp has to be stored, and baselines
    // written by older versions report one too. A CI cache that restores the
    // file without its mtime reports the restore instead.
    fn last_marked(&self, consumer: &str) -> Option<u64> {
        let modified = std::fs::metadata(self.state_path(consumer))
            .and_then(|meta| meta.modified())
            .ok()?;
        Some(
            modified
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs(),
        )
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

    pub fn consumers(&self) -> Result<Vec<String>> {
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
