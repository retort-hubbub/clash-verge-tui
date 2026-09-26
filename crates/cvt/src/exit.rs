//! Exit codes.
//!
//! The numbers are a contract: a script that runs `clash-verge-tui` branches on
//! them, so they are defined once here, documented in `--help` and in
//! `docs/CLI.md`, and never invented at a call site.
//!
//! Mapping a library error onto a code is deliberately conservative: only the
//! three conditions the codes name get their own number, and everything else
//! is a generic failure. "The controller answered with 400" is not "the
//! controller is unreachable", and claiming it is would send a script down the
//! wrong branch.

use std::fmt;

/// The process exit status of a `clash-verge-tui` invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    /// The command did what was asked.
    Success = 0,
    /// A failure no other code describes.
    Failure = 1,
    /// The command line was wrong. Produced by `clap` itself.
    Usage = 2,
    /// The generated configuration failed validation.
    Validation = 3,
    /// The controller is unreachable, or the core is not running.
    Controller = 4,
    /// No `mihomo` binary was found.
    NoCore = 5,
}

impl ExitCode {
    /// The number the process exits with.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The one-line meaning, as printed by `--help` and in reports.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "a generic failure",
            Self::Usage => "invalid usage",
            Self::Validation => "the generated configuration failed validation",
            Self::Controller => "the controller is unreachable or the core is not running",
            Self::NoCore => "no core binary was found",
        }
    }

    /// The code a failed library operation must produce.
    #[must_use]
    pub fn for_core(error: &cvt_core::Error) -> Self {
        match error {
            cvt_core::Error::Validation { .. } => Self::Validation,
            cvt_core::Error::ControllerUnreachable { .. } => Self::Controller,
            cvt_core::Error::CoreUnavailable { .. } => Self::NoCore,
            // `Api` is *not* `Controller`: the core answered, so it is running.
            // `ProcessFailed` covers both a rejected config and a binary that
            // would not spawn, and guessing between them from the message
            // would be worse than reporting a generic failure.
            _ => Self::Failure,
        }
    }

    /// The code for a failure that reached `main`.
    ///
    /// The whole `anyhow` chain is searched, because the useful error is
    /// usually the one a `.context(...)` is wrapped around, not the context.
    #[must_use]
    pub fn for_error(error: &anyhow::Error) -> Self {
        for cause in error.chain() {
            if let Some(exit) = cause.downcast_ref::<Exit>() {
                return exit.code;
            }
            if let Some(core) = cause.downcast_ref::<cvt_core::Error>() {
                return Self::for_core(core);
            }
        }
        Self::Failure
    }
}

/// An error whose whole content is an exit code and a reason to print.
///
/// Commands whose *output* is the failure — `doctor` above all — have no
/// library error to report, but the process still has to exit non-zero.
#[derive(Debug, Clone)]
pub struct Exit {
    /// The code to exit with.
    pub code: ExitCode,
    /// What to print on stderr.
    pub message: String,
}

impl Exit {
    /// A failure carrying an explanation.
    #[must_use]
    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            code: ExitCode::Failure,
            message: message.into(),
        }
    }

    /// A failure carrying one of the documented codes.
    #[must_use]
    pub fn new(code: ExitCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Exit {}

/// The block appended to `--help`, so the codes are discoverable where a user
/// looks first.
pub const EXIT_CODES_HELP: &str = "\
Exit codes:
  0  success
  1  a generic failure
  2  invalid usage
  3  the generated configuration failed validation
  4  the controller is unreachable or the core is not running
  5  no core binary was found

Every command that only reads accepts --json and prints one JSON object on
stdout whose `schema` field names its shape. Diagnostics and progress always go
to stderr, so stdout stays pipeable. See docs/CLI.md for every schema.";

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cvt_core::Error;

    #[test]
    fn the_numbers_are_the_documented_ones() {
        assert_eq!(ExitCode::Success.as_u8(), 0);
        assert_eq!(ExitCode::Failure.as_u8(), 1);
        assert_eq!(ExitCode::Usage.as_u8(), 2);
        assert_eq!(ExitCode::Validation.as_u8(), 3);
        assert_eq!(ExitCode::Controller.as_u8(), 4);
        assert_eq!(ExitCode::NoCore.as_u8(), 5);
        assert_eq!(
            ExitCode::Usage.as_u8(),
            2,
            "clap must agree with this table"
        );
    }

    #[test]
    fn library_errors_map_onto_the_specific_codes() {
        assert_eq!(
            ExitCode::for_core(&Error::Validation {
                problems: vec!["boom".into()]
            }),
            ExitCode::Validation
        );
        assert_eq!(
            ExitCode::for_core(&Error::ControllerUnreachable {
                endpoint: "127.0.0.1:9090".into(),
                source: "refused".into(),
            }),
            ExitCode::Controller
        );
        assert_eq!(
            ExitCode::for_core(&Error::CoreUnavailable {
                reason: "nowhere".into()
            }),
            ExitCode::NoCore
        );
    }

    #[test]
    fn everything_else_is_a_generic_failure() {
        // A rejected API call means the controller answered; that is not code 4.
        assert_eq!(
            ExitCode::for_core(&Error::Api {
                method: "GET",
                path: "/proxies".into(),
                status: 400,
                body: "nope".into(),
            }),
            ExitCode::Failure
        );
        assert_eq!(
            ExitCode::for_core(&Error::ProfileNotFound { uid: "x".into() }),
            ExitCode::Failure
        );
        assert_eq!(ExitCode::for_core(&Error::Cancelled), ExitCode::Failure);
    }

    #[test]
    fn a_context_wrapped_error_still_maps() {
        let core = Error::CoreUnavailable {
            reason: "nowhere".into(),
        };
        let wrapped = anyhow::Error::new(core).context("while starting the core");
        assert_eq!(
            ExitCode::for_error(&wrapped),
            ExitCode::NoCore,
            "the context must not hide the cause"
        );

        let deep = anyhow::Error::new(Error::Validation { problems: vec![] })
            .context("inner")
            .context("outer");
        assert_eq!(ExitCode::for_error(&deep), ExitCode::Validation);
    }

    #[test]
    fn an_explicit_exit_wins_over_everything_else() {
        let err = anyhow::Error::new(Exit::new(ExitCode::Controller, "the core is not running"))
            .context("doctor");
        assert_eq!(ExitCode::for_error(&err), ExitCode::Controller);
    }

    #[test]
    fn an_unrecognised_error_is_a_generic_failure() {
        let err = anyhow::anyhow!("something else entirely");
        assert_eq!(ExitCode::for_error(&err), ExitCode::Failure);
    }

    #[test]
    fn the_help_block_names_every_code() {
        for code in [
            ExitCode::Success,
            ExitCode::Failure,
            ExitCode::Validation,
            ExitCode::Controller,
            ExitCode::NoCore,
        ] {
            assert!(
                EXIT_CODES_HELP.contains(code.description()),
                "{} is missing from the help block",
                code.description()
            );
        }
        assert!(EXIT_CODES_HELP.contains("--json"));
    }
}
