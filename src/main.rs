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

mod adf;
mod api;
mod boards;
mod config;
mod mirror;
mod orgs;
mod projects;
mod show;
mod sprints;
mod tickets;

use std::sync::OnceLock;


const USAGE: &str = "\
usage: myjira <command> [options]

  list-orgs               list the configured orgs, with the flavour, cloud id
                          and the account your session belongs to
  list-projects           list the projects of one org
  list-sprints <PROJECT>  list the sprints of a project's boards
  list-tickets <PROJECT> [<SPRINT ID>|backlog]
                          list a project's tickets, or only one sprint's, or
                          the ones on its boards and in no sprint
  show <TICKET>           show one ticket: its fields, description and comments

options:
      --org <name>        which org to act on (also --org=<name>). Without it:
                          DEFAULT_ORG, or the only org configured.
      --mirror <dir>      also write each loaded ticket to
                          <dir>/<PROJECT>/<TICKET>.md. With list-tickets that
                          fetches every listed ticket, two calls apiece.
                          Write-only — files are overwritten, never deleted.
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
pub const YELLOW: &str = "\x1b[33m";
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
    // Where to mirror what was loaded, if anywhere.
    pub mirror: Option<mirror::Mirror>,
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
    let mut mirror_root: Option<String> = None;
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
            "--mirror" => {
                mirror_root = Some(args.next().ok_or("missing directory for --mirror (e.g. --mirror=~/jira)")?);
            }
            other if other.starts_with("--mirror=") => {
                mirror_root = Some(other.trim_start_matches("--mirror=").to_string());
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

    let opts = Opts { verbose, json, mirror: mirror_root.as_deref().map(mirror::Mirror::new) };
    // Said rather than ignored: a flag that silently does nothing hides a typo
    // the same way an unknown one would. Only a ticket has content to write.
    if opts.mirror.is_some() && words[0] != "show" && words[0] != "list-tickets" {
        eprintln!("--mirror only applies to show and list-tickets — nothing was written");
    }
    // Silently ignoring a stray word would hide a typo the same way an unknown
    // flag would: `list-sprints BUA GDE` looks like it listed both.
    let expected = match words[0].as_str() {
        "list-sprints" | "show" => 2,
        "list-tickets" => 3,
        _ => 1,
    };
    if words.len() > expected {
        return Err(format!("unexpected argument: {}\n\n{USAGE}", words[expected]).into());
    }

    match words[0].as_str() {
        "list-orgs" => orgs::list(&config::load(env_path.as_deref())?, &opts).await,
        "list-projects" => {
            let config = config::load(env_path.as_deref())?;
            projects::list(config.pick(org.as_deref())?, &opts).await
        }
        "show" => {
            let ticket = words.get(1).ok_or("show needs a ticket key, e.g. myjira show BTE-915")?;
            let config = config::load(env_path.as_deref())?;
            show::show(config.pick(org.as_deref())?, ticket, &opts).await
        }
        "list-tickets" => {
            let project = words
                .get(1)
                .ok_or("list-tickets needs a project key, e.g. myjira list-tickets BUA [<sprint id>|backlog]")?;
            let config = config::load(env_path.as_deref())?;
            let instance = config.pick(org.as_deref())?;
            tickets::list(instance, project, words.get(2).map(String::as_str), &opts).await
        }
        "list-sprints" => {
            let project = words
                .get(1)
                .ok_or("list-sprints needs a project key, e.g. myjira list-sprints BUA")?;
            let config = config::load(env_path.as_deref())?;
            sprints::list(config.pick(org.as_deref())?, project, &opts).await
        }
        other => Err(format!("unknown command: {other}\n\n{USAGE}").into()),
    }
}

