// println! panics when the reader goes away, and `myjira list-projects | head`
// or `| fzf` is a normal way to use this — a panic with a backtrace is not the
// right answer to a pipe that has simply been closed. Defined before the
// modules so they can use it.
#[macro_export]
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        if let Err(e) = writeln!(std::io::stdout(), $($arg)*) {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                // What the downstream tool expects: stop, quietly, successfully.
                std::process::exit(0);
            }
            eprintln!("myjira: cannot write to stdout: {e}");
            std::process::exit(1);
        }
    }};
}

mod api;
mod config;
mod orgs;
mod projects;

use std::sync::OnceLock;


const USAGE: &str = "\
usage: myjira <command> [options]

  list-orgs               list the configured orgs, with the flavour, cloud id
                          and the account your session belongs to
  list-projects           list the projects of one org

options:
      --org <name>        which org to act on (also --org=<name>). Without it:
                          DEFAULT_ORG, or the only org configured.
      --json              print the result as JSON instead of a table
      --env <file>        read the configuration from this file
                          (default: ~/.config/myjira/.env)
      --no-color          plain output without ANSI colours
  -v, --verbose           report progress while running
  -h, --help              show this message
";

pub const BOLD: &str = "\x1b[1m";
pub const DIM: &str = "\x1b[2m";
pub const CYAN: &str = "\x1b[36m";
pub const RED: &str = "\x1b[31m";
pub const RESET: &str = "\x1b[0m";

// Set once at startup: color is on by default, off with --no-color. (Tools like
// fzf display ANSI-colored input fine via their own --ansi flag, so this
// doesn't need to auto-disable based on whether stdout is a TTY.)
static COLOR_ENABLED: OnceLock<bool> = OnceLock::new();

fn color_enabled() -> bool {
    *COLOR_ENABLED.get().unwrap_or(&false)
}

// Returns `code` if color is enabled, otherwise "" — wrap every color/reset
// constant with this instead of using them directly.
pub fn c(code: &'static str) -> &'static str {
    if color_enabled() { code } else { "" }
}

pub struct Opts {
    pub verbose: bool,
    pub json: bool,
}

type Error = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() {
    // Reported here rather than by returning Err from main: main's own error
    // reporting uses Debug formatting, which renders a message with quotes and
    // any usage text with literal escapes.
    if let Err(e) = run().await {
        eprintln!("myjira: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Error> {
    let mut verbose = false;
    let mut no_color = false;
    let mut json = false;
    let mut env_path: Option<String> = None;
    let mut org: Option<String> = None;
    // The group/command pair, e.g. ("org", "list"). Collected positionally so
    // flags can sit on either side of it.
    let mut words: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-v" | "--verbose" => verbose = true,
            "--no-color" => no_color = true,
            "--json" => json = true,
            "--env" => {
                env_path = Some(args.next().ok_or("missing value for --env (e.g. --env=/path/.env)")?);
            }
            other if other.starts_with("--env=") => {
                env_path = Some(other.trim_start_matches("--env=").to_string());
            }
            "--org" => {
                org = Some(args.next().ok_or("missing value for --org (e.g. --org=digitalklang)")?);
            }
            other if other.starts_with("--org=") => {
                org = Some(other.trim_start_matches("--org=").to_string());
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            // Silently ignoring an unknown flag hides typos: a mistyped --json
            // would mean a run that looks fine and prints the wrong thing.
            other if other.starts_with('-') => {
                eprintln!("unknown argument: {other}\n");
                eprint!("{USAGE}");
                std::process::exit(2);
            }
            other => words.push(other.to_string()),
        }
    }
    COLOR_ENABLED.set(!no_color).ok();

    if words.is_empty() {
        print!("{USAGE}");
        return Ok(());
    }

    let opts = Opts { verbose, json };

    match words[0].as_str() {
        "list-orgs" => orgs::list(&config::load(env_path.as_deref())?, &opts).await,
        "list-projects" => {
            let config = config::load(env_path.as_deref())?;
            projects::list(config.pick(org.as_deref())?, &opts).await
        }
        other => Err(format!("unknown command: {other}\n\n{USAGE}").into()),
    }
}

