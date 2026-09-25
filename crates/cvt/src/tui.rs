//! The interactive interface.
//!
//! `cvt-tui` owns everything that touches a terminal, and it does not expose a
//! run entry point yet. This module is the only place in the binary that knows
//! that: when the interface lands, this function grows a body and nothing else
//! changes.

use anyhow::Result;

use crate::exit::Exit;
use crate::output::Output;

/// Start the terminal interface.
///
/// # Errors
/// Always, until `cvt-tui` grows an entry point. The process must exit
/// non-zero rather than report success for something that never started.
pub fn run(out: Output) -> Result<()> {
    out.note(
        "cvt-tui does not expose a run entry point yet; every CLI subcommand works — \
         try `clash-verge-tui status`, `clash-verge-tui doctor` or `clash-verge-tui --help`",
    );
    Err(Exit::failure("the interactive interface is not available in this build").into())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_missing_interface_is_reported_rather_than_ignored() {
        let error = run(Output::new(true, 0, false)).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("not available in this build"),
            "the message has to say what is missing: {message}"
        );
    }
}
