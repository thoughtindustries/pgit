mod commands;
mod config;
mod envfile;
mod git;
mod naming;
mod pg;

use clap::{Parser, Subcommand};
use std::io::IsTerminal;

#[derive(Parser)]
#[command(version, about = "Per-branch Postgres databases")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a branch database from the template and switch to it
    Create {
        /// Branch name (defaults to the current git branch)
        branch: Option<String>,
        /// Clone another branch's database instead of the template
        #[arg(long, value_name = "BRANCH")]
        from: Option<String>,
        /// Terminate active connections to the source database
        #[arg(short, long)]
        force: bool,
    },
    /// Switch to an existing branch database
    Use {
        /// Branch name (defaults to the current git branch)
        branch: Option<String>,
    },
    /// Drop a branch database
    Drop {
        /// Branch name (defaults to the current git branch)
        branch: Option<String>,
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    /// List branch databases
    List,
    /// Remove the override and return to the template database
    Reset,
    /// Show config, active database and the database for the current branch
    #[command(alias = "current")]
    Status,
    /// Write a starter pgit.yaml
    Init,
    /// Drop branch databases whose git branch no longer exists
    Prune {
        /// Don't ask for confirmation
        #[arg(short, long)]
        yes: bool,
        /// Only list what would be dropped
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the post-checkout hook line, or install/run it
    Hook {
        #[command(subcommand)]
        action: Option<HookCmd>,
    },
}

#[derive(Subcommand)]
enum HookCmd {
    /// Install a post-checkout hook in this repo
    Install,
    /// Run from post-checkout (receives git's hook arguments)
    Run {
        prev: Option<String>,
        new: Option<String>,
        flag: Option<String>,
    },
}

fn color(code: &str, label: &str) -> String {
    if std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        format!("\x1b[1;{code}m{label}\x1b[0m")
    } else {
        label.to_string()
    }
}

pub fn info(msg: &str) {
    eprintln!("{}  {msg}", color("34", "[info]"));
}
pub fn ok(msg: &str) {
    eprintln!("{}    {msg}", color("32", "[ok]"));
}
pub fn warn(msg: &str) {
    eprintln!("{}  {msg}", color("33", "[warn]"));
}

fn main() {
    // Rust ignores SIGPIPE, so `pgit list | head` would panic; die quietly like other CLIs.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let result = match Cli::parse().cmd {
        Cmd::Create {
            branch,
            from,
            force,
        } => commands::create(branch, from, force),
        Cmd::Use { branch } => commands::use_(branch),
        Cmd::Drop { branch, yes } => commands::drop(branch, yes),
        Cmd::List => commands::list(),
        Cmd::Reset => commands::reset(),
        Cmd::Status => commands::status(),
        Cmd::Init => commands::init(),
        Cmd::Prune { yes, dry_run } => commands::prune(yes, dry_run),
        Cmd::Hook { action: None } => {
            commands::hook_print();
            Ok(())
        }
        Cmd::Hook {
            action: Some(HookCmd::Install),
        } => commands::hook_install(),
        Cmd::Hook {
            action: Some(HookCmd::Run { flag, .. }),
        } => {
            commands::hook_run(flag);
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("{} {e:#}", color("31", "[error]"));
        std::process::exit(1);
    }
}
