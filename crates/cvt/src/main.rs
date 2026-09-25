//! The `clash-verge-tui` command line.
//!
//! `main` does three things: parse the arguments, dispatch to the module that
//! implements the command, and turn whatever comes back into one of the
//! documented exit codes. Everything else lives in a module of its own:
//!
//! * [`cli`] — the whole surface a user can type,
//! * [`commands`] — one module per subcommand group,
//! * [`context`] — the opened application, and the single write path,
//! * [`output`] — stdout as a value, stderr as diagnostics,
//! * [`exit`] — the exit-code contract,
//! * [`tui`] — the interactive interface, when there is one.

#![warn(missing_docs)]
// A binary crate has no external consumers, so every `pub` item is
// "unreachable" by construction and this lint fires on all of them. The
// alternative, `pub(crate)` on every item, trips `clippy::redundant_pub_crate`
// (nursery) on all of them instead — the two lints contradict each other here.
// Items are written and documented as if they were public, because they are
// meant to be read.
#![allow(unreachable_pub)]

mod cli;
mod commands;
mod context;
mod exit;
mod output;
mod tui;

use std::io::IsTerminal as _;
use std::process::ExitCode as ProcessExit;

use clap::Parser as _;
use cvt_core::AppPaths;

use crate::cli::{Cli, Command};
use crate::context::Ctx;
use crate::exit::ExitCode;
use crate::output::Output;

#[tokio::main]
async fn main() -> ProcessExit {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            // clap renders help and version to stdout with a success status,
            // and a real usage error to stderr with code 2. `use_stderr` is
            // how it distinguishes them.
            let _ = error.print();
            let code = if error.use_stderr() {
                ExitCode::Usage
            } else {
                ExitCode::Success
            };
            return ProcessExit::from(code.as_u8());
        }
    };
    // `NO_COLOR` is honoured because a user who sets it means it; `--no-color`
    // is for the case where they cannot.
    let color = !cli.no_color && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
    let verbose = cli.verbose;
    let out = Output::new(cli.json, verbose, color);
    init_tracing(verbose, color);

    match run(cli, out).await {
        Ok(()) => ProcessExit::from(ExitCode::Success.as_u8()),
        Err(error) => {
            // The whole chain, so a failure deep inside the library is not
            // reduced to "could not open the application home".
            eprintln!("error: {error:#}");
            let code = ExitCode::for_error(&error);
            if verbose > 0 {
                eprintln!("exit {} ({})", code.as_u8(), code.description());
            }
            ProcessExit::from(code.as_u8())
        }
    }
}

/// What a command line asks for, decided before anything is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// `doctor` reports on the home as it found it.
    Doctor,
    /// Everything else is handed an opened home.
    Command,
    /// No subcommand: the interactive interface.
    Interface,
}

/// Decide how a command line has to be dispatched.
///
/// Opening the application home creates the directory layout, so `doctor` —
/// whose first check is whether that layout is there — must be routed before
/// anything is opened.
fn route(command: Option<&Command>) -> Route {
    match command {
        None => Route::Interface,
        Some(Command::Doctor) => Route::Doctor,
        Some(_) => Route::Command,
    }
}

/// Resolve the home and hand the command to the module that implements it.
async fn run(cli: Cli, out: Output) -> anyhow::Result<()> {
    let paths = AppPaths::resolve(cli.home.as_deref())?;
    match (route(cli.command.as_ref()), cli.command) {
        (Route::Doctor, _) => commands::doctor::run(&paths, out).await,
        (Route::Command, Some(command)) => {
            commands::dispatch(&Ctx::open(paths, out)?, &command).await
        }
        // Only `None` routes to the interface, so the remaining shapes are
        // unreachable; handling them keeps `run` total without a panic.
        _ => tui::run(out),
    }
}

/// Send `tracing` output to stderr, when the caller asked for it.
///
/// Without `-v` the program is silent: a `tracing` event from `cvt-core` is a
/// diagnostic, and diagnostics must never land on the stdout that `--json`
/// output is piped from.
fn init_tracing(verbose: u8, color: bool) {
    if verbose == 0 {
        return;
    }
    let level = match verbose {
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!("cvt={level},cvt_core={level}"))
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(color && std::io::stderr().is_terminal())
        .with_target(verbose > 1)
        .try_init();
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    #[test]
    fn doctor_is_routed_before_anything_is_opened() {
        let cli = parse(&["clash-verge-tui", "doctor"]);
        assert_eq!(route(cli.command.as_ref()), Route::Doctor);
    }

    #[test]
    fn every_other_command_gets_an_opened_home() {
        let cli = parse(&["clash-verge-tui", "status"]);
        assert_eq!(route(cli.command.as_ref()), Route::Command);
        let cli = parse(&["clash-verge-tui", "profiles", "list"]);
        assert_eq!(route(cli.command.as_ref()), Route::Command);
    }

    #[test]
    fn no_subcommand_means_the_interface() {
        let cli = parse(&["clash-verge-tui"]);
        assert_eq!(route(cli.command.as_ref()), Route::Interface);
    }

    #[test]
    fn opening_a_home_creates_the_layout_the_commands_use() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = dir.path().join("fresh");
        let ctx = Ctx::open(AppPaths::new(&home), Output::new(false, 0, false)).unwrap();
        assert!(home.is_dir());
        assert_eq!(ctx.paths().home(), home);
        assert!(ctx.paths().profiles_dir().is_dir());
    }
}
