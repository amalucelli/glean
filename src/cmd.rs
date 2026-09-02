// One function per subcommand: what each reads, what it writes, and to which
// stream.
//
// The division of streams is the contract the examples in `--help` depend on.
// A change set goes to stdout as plain bytes so it can be piped straight into
// the next tool; counts and notices go to stderr, so a `list | mark` pipeline
// carries paths and nothing else. Exit codes carry the same discipline: `list
// -q` returns 1 for "nothing changed", which is why a real failure exits 2.

use crate::ink::{marked_files, removed, Ink};
use crate::repo::Repo;
use crate::DEFAULT_CONSUMER;
use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::process::Command;

pub fn list(consumer: &str, null: bool, quiet: bool, json: bool) -> Result<i32> {
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

pub fn mark(consumer: &str, paths: &[String], stdin: bool, null: bool) -> Result<i32> {
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
    eprintln!("{}", marked_files(marked));
    Ok(0)
}

pub fn status(consumer: Option<&str>, json: bool) -> Result<i32> {
    let repo = Repo::discover(consumer.unwrap_or(DEFAULT_CONSUMER))?;
    let ink = Ink::stdout();
    let names = match consumer {
        Some(name) => vec![name.to_string()],
        None => {
            let consumers = repo.consumers()?;
            if consumers.is_empty() {
                if json {
                    println!("[]");
                } else {
                    println!("{}", ink.dim("no glean state for any consumer"));
                }
                return Ok(0);
            }
            consumers
        }
    };

    let statuses = repo.status_for(&names)?;
    if json {
        println!("{}", serde_json::to_string(&statuses)?);
    } else {
        for s in &statuses {
            let changed = match s.changed {
                0 => ink.dim("0"),
                n => ink.yellow(&n.to_string()),
            };
            println!(
                "{}: {} {} {} {}",
                ink.cyan(&s.consumer),
                ink.bold(&s.tracked.to_string()),
                ink.dim("tracked,"),
                changed,
                ink.dim("changed")
            );
        }
    }
    Ok(0)
}

pub fn reset(consumer: &str, all: bool) -> Result<i32> {
    let repo = Repo::discover(consumer)?;
    if all {
        // Drop the whole dir, not just the files, so nothing lingers in .git.
        let dir = repo.git_dir.join("glean");
        if std::fs::remove_dir_all(&dir).is_ok() {
            eprintln!("{}", removed(&dir));
        }
        return Ok(0);
    }
    let path = repo.state_path(consumer);
    if std::fs::remove_file(&path).is_ok() {
        eprintln!("{}", removed(&path));
    }
    Ok(0)
}

// Snapshot the changed set once, run the command on it, mark on success. One
// process means no window between deciding the work and recording it done.
pub fn run_cmd(consumer: &str, each: bool, command: &[String]) -> Result<i32> {
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
        eprintln!("{}", marked_files(marked));
        Ok(if ok.len() == files.len() { 0 } else { 1 })
    } else {
        let status = spawn(command, &files)?;
        if status.success() {
            let marked = repo.mark_paths(&files)?;
            eprintln!("{}", marked_files(marked));
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
