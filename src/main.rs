// Per-repo incremental change tracker. Multiple independent consumers (cleanup
// skills run by separate agent loops) each keep their own baseline of which file
// contents they've already processed; when a file changes, every consumer that
// hasn't seen those exact bytes sees it again. State lives in the git dir, never
// committed, keyed per consumer so concurrent consumers are each the sole writer
// of their own file and no locking is needed.
//
// The split: `repo` owns the baselines and the git queries behind them, `cmd`
// owns one function per subcommand and the streams each writes to, `ink` owns
// colour, and `mcp` exposes the same operations as tools over stdio. What is
// left here is the argument grammar.

mod cmd;
mod ink;
mod mcp;
mod repo;

pub use repo::Repo;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{ArgAction, Parser, Subcommand};
use ink::Ink;

// `task install` sets GLEAN_DIRTY so a locally built binary is distinguishable
// from a released one that happens to carry the same crate version.
const VERSION: &str = match option_env!("GLEAN_DIRTY") {
    Some(_) => concat!(env!("CARGO_PKG_VERSION"), "-dirty"),
    None => env!("CARGO_PKG_VERSION"),
};

const DEFAULT_CONSUMER: &str = "default";

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default());

const EXAMPLES: &str = "\
Examples:
  # hand the changed set to a consumer, then record exactly what it saw
  glean list -z --as slop | glean mark --stdin -z --as slop

  # gate a whole-project tool that cannot take file paths
  glean list -q --as clippy && cargo clippy && glean mark --as clippy
";

#[derive(Parser)]
#[command(
    about = "Track which files changed since a tool last processed them.",
    version = VERSION,
    // clap's default version flag takes -V; -v is glean's, so the default has to go.
    disable_version_flag = true,
    disable_help_subcommand = true,
    styles = STYLES,
    after_help = EXAMPLES,
)]
struct Cli {
    #[command(subcommand)]
    command: Sub,

    /// Consumer whose baseline to read or advance [default: default]
    #[arg(long = "as", global = true, value_name = "NAME")]
    consumer: Option<String>,

    /// Print version
    #[arg(short = 'v', long, action = ArgAction::Version)]
    version: (),
}

#[derive(Subcommand)]
enum Sub {
    /// Files changed since this consumer's last mark
    List {
        /// Separate paths with NUL instead of newline
        #[arg(short = 'z', long, conflicts_with = "json")]
        null: bool,
        /// Print nothing; exit 0 if anything changed, 1 if not
        #[arg(short, long)]
        quiet: bool,
        /// Write the paths as a JSON array
        #[arg(long)]
        json: bool,
    },
    /// Record files as processed; with no paths, the whole changed set
    Mark {
        /// Read the paths to mark from stdin
        #[arg(long)]
        stdin: bool,
        /// Expect NUL-separated paths on stdin instead of newline-separated
        #[arg(short = 'z', long)]
        null: bool,
        #[arg(value_name = "PATH")]
        paths: Vec<String>,
    },
    /// Tracked and changed counts; with no --as, every consumer
    Status {
        /// Write the counts as a JSON array
        #[arg(long)]
        json: bool,
    },
    /// Forget a baseline to force a full re-sweep
    Reset {
        /// Forget every consumer's baseline
        #[arg(long)]
        all: bool,
    },
    /// Run a command on the changed files, marking them when it succeeds
    Run {
        /// Run the command once per file, marking only the files that pass
        #[arg(long)]
        each: bool,
        #[arg(required = true, last = true, value_name = "CMD")]
        command: Vec<String>,
    },
    /// Serve the change-set as MCP tools over stdio
    Mcp,
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        // Runtime failures exit 2 so `list -q`'s exit 1 stays an unambiguous
        // "no changes" signal and never collides with an error.
        Err(err) => {
            eprintln!("{} {err:#}", Ink::stderr().red("error:"));
            2
        }
    };
    std::process::exit(code);
}

fn run() -> anyhow::Result<i32> {
    let cli = Cli::parse();
    let consumer = cli.consumer.as_deref().unwrap_or(DEFAULT_CONSUMER);

    match cli.command {
        Sub::List { null, quiet, json } => cmd::list(consumer, null, quiet, json),
        Sub::Mark { stdin, null, paths } => cmd::mark(consumer, &paths, stdin, null),
        Sub::Status { json } => cmd::status(cli.consumer.as_deref(), json),
        Sub::Reset { all } => cmd::reset(consumer, all),
        Sub::Run { each, command } => cmd::run_cmd(consumer, each, &command),
        Sub::Mcp => mcp::serve(),
    }
}
