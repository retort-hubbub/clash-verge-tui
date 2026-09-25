//! The interactive interface.
//!
//! A thin adapter, on purpose. `cvt-tui` owns the terminal, the event loop and
//! the state machine; this module's whole job is to open the application, turn
//! the user's settings into a theme, and hand the loop something that can
//! perform its effects.
//!
//! The effect executor is where the interface meets the same `Service` the
//! command line uses — see [`crate::executor`]. That is what keeps the two
//! front ends from drifting apart: neither of them implements an operation,
//! they only ask for one.

use anyhow::{Context as _, Result};
use cvt_core::{AppPaths, Service};
use cvt_tui::Theme;

use crate::executor::Executor;
use crate::output::Output;

/// Start the terminal interface.
///
/// # Errors
/// Propagates a home that cannot be opened, and
/// [`cvt_tui::RunError`] when there is no terminal to draw on — the interface
/// must fail loudly rather than write escape sequences into a pipe and report
/// success.
pub async fn run(paths: &AppPaths, out: Output) -> Result<()> {
    // Opened once and shared: the executor needs `&mut` for the settings, so
    // the service lives behind a lock inside the executor rather than being
    // cloned.
    let service = Service::open(paths.clone()).context("could not open the application home")?;
    let theme = Theme::from_settings(service.settings().ui.color);
    let home = paths.home().to_path_buf();
    let executor = Executor::new(service).into_executor();

    out.note("starting the interactive interface; press ? for help, q to quit");
    cvt_tui::run::run(home, theme, executor).await?;
    Ok(())
}

/// Open the interface against an already-resolved home.
///
/// Used by the tests below, and by anything that needs to drive the interface
/// without going through argument parsing.
///
/// # Errors
/// As [`run`].
#[cfg(test)]
pub async fn run_home(home: &std::path::Path) -> Result<()> {
    let paths = AppPaths::new(home);
    run(&paths, Output::new(false, 0, false)).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The interface must not report success for something that never started.
    ///
    /// Output here is a test harness' pipe rather than a terminal, which is
    /// exactly the case the check exists for: a TUI that silently wrote escape
    /// sequences into a pipe would look like a successful run.
    #[tokio::test]
    async fn a_pipe_is_reported_rather_than_written_into() {
        let dir = tempfile::TempDir::new().unwrap();
        let error = run_home(dir.path()).await.unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("terminal"),
            "the message has to say what is missing: {message}"
        );
    }

    /// And the failure is the one the exit-code mapping already understands.
    #[test]
    fn the_missing_terminal_is_a_failure_exit() {
        let dir = tempfile::TempDir::new().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime.block_on(run_home(dir.path())).unwrap_err();
        let exit = crate::exit::ExitCode::for_error(&error);
        assert_eq!(exit, crate::exit::ExitCode::Failure);
    }

    #[test]
    fn the_executor_takes_ownership_of_the_service() {
        let dir = tempfile::TempDir::new().unwrap();
        let service = Service::open(AppPaths::new(dir.path())).unwrap();
        let _executor = Executor::new(service).into_executor();
    }
}
