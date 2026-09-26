//! Error types for `cvt-core`.
//!
//! The crate exposes a single [`Result`] alias. Every fallible operation returns
//! [`Error`], which carries enough context to be rendered directly in the TUI
//! status line without further formatting.

use std::path::PathBuf;

/// Convenient result alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong inside `cvt-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem access failed.
    #[error("io error at {path}: {source}")]
    Io {
        /// Path that was being accessed.
        path: PathBuf,
        /// Underlying cause.
        #[source]
        source: std::io::Error,
    },

    /// A file was read successfully but could not be parsed.
    #[error("failed to parse {kind} at {path}: {source}")]
    Parse {
        /// Human readable description of the document kind.
        kind: &'static str,
        /// Path of the offending document.
        path: PathBuf,
        /// Underlying cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Serialisation failed while generating a config.
    #[error("failed to serialise {kind}: {source}")]
    Serialize {
        /// Human readable description of the document kind.
        kind: &'static str,
        /// Underlying cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// An HTTP request to a subscription URL or the core API failed.
    #[error("http request to {url} failed: {source}")]
    Http {
        /// Target URL.
        url: String,
        /// Underlying cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The core API answered with a non-success status.
    #[error("core api {method} {path} returned {status}: {body}")]
    Api {
        /// HTTP method.
        method: &'static str,
        /// Request path, relative to the controller root.
        path: String,
        /// HTTP status code.
        status: u16,
        /// Response body, truncated for display.
        body: String,
    },

    /// The controller could not be reached at all.
    #[error("cannot reach the mihomo controller at {endpoint}: {source}")]
    ControllerUnreachable {
        /// Endpoint that was dialled (`host:port` or unix socket path).
        endpoint: String,
        /// Underlying cause.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// A profile referenced by the store does not exist.
    #[error("profile {uid} not found")]
    ProfileNotFound {
        /// Missing profile uid.
        uid: String,
    },

    /// The profile chain contains a cycle or is otherwise unresolvable.
    #[error("profile chain is invalid: {reason}")]
    InvalidChain {
        /// Explanation of the fault.
        reason: String,
    },

    /// A profile is missing a field required for the requested operation.
    #[error("profile {uid} is missing required field `{field}`")]
    MissingField {
        /// Profile uid.
        uid: String,
        /// Name of the absent field.
        field: &'static str,
    },

    /// The merged runtime configuration failed validation.
    #[error("configuration validation failed:\n{}", .problems.join("\n"))]
    Validation {
        /// Individual problems, one per line.
        problems: Vec<String>,
    },

    /// The core binary could not be located or launched.
    #[error("mihomo core unavailable: {reason}")]
    CoreUnavailable {
        /// Explanation, including the paths that were searched.
        reason: String,
    },

    /// A subprocess exited unsuccessfully.
    #[error("{program} exited with status {status}")]
    ProcessFailed {
        /// Program name.
        program: String,
        /// Exit status, formatted.
        status: String,
        /// Captured stderr, truncated for display.
        stderr: String,
    },

    /// A value from user input or a config file was out of range or malformed.
    #[error("invalid value for {field}: {reason}")]
    InvalidValue {
        /// Field or setting name.
        field: &'static str,
        /// Why the value was rejected.
        reason: String,
    },

    /// The operation was cancelled by the user or superseded by a newer one.
    #[error("operation cancelled")]
    Cancelled,

    /// The feature is not supported for the given input.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl Error {
    /// Attach a path to an [`std::io::Error`], producing [`Error::Io`].
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Build a [`Error::Parse`] from any error type.
    pub fn parse(
        kind: &'static str,
        path: impl Into<PathBuf>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Parse {
            kind,
            path: path.into(),
            source: Box::new(source),
        }
    }

    /// Build a [`Error::Serialize`] from any error type.
    pub fn serialize(
        kind: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Serialize {
            kind,
            source: Box::new(source),
        }
    }

    /// Build a [`Error::Http`] from any error type.
    pub fn http(
        url: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Http {
            url: url.into(),
            source: Box::new(source),
        }
    }

    /// Build a [`Error::InvalidValue`] with a formatted reason.
    pub fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidValue {
            field,
            reason: reason.into(),
        }
    }

    /// `true` when retrying the identical operation could plausibly succeed.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Http { .. } | Self::ControllerUnreachable { .. })
    }

    /// `true` when restoring the previous configuration could plausibly help.
    ///
    /// A document the core *refused* is worth undoing: the previous one was
    /// working. A machine with no core binary, or an operation this core does
    /// not support, is not — the previous document would fail in exactly the
    /// same way, and undoing it would only destroy the evidence.
    #[must_use]
    pub fn is_rollbackable(&self) -> bool {
        !matches!(self, Self::CoreUnavailable { .. } | Self::Unsupported(_))
    }

    /// A short, single-line rendering suitable for a TUI status bar.
    #[must_use]
    pub fn short(&self) -> String {
        let full = self.to_string();
        let first = full.lines().next().unwrap_or_default();
        if first.chars().count() > 120 {
            let mut s: String = first.chars().take(117).collect();
            s.push_str("...");
            s
        } else {
            first.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_refused_document_is_worth_rolling_back() {
        // The previous document would fail in exactly the same way.
        assert!(
            !Error::CoreUnavailable {
                reason: "searched: /usr/bin".to_owned()
            }
            .is_rollbackable()
        );
        assert!(!Error::Unsupported("no /script route".to_owned()).is_rollbackable());

        // Each of these could plausibly be fixed by restoring what worked.
        assert!(
            Error::ProcessFailed {
                program: "mihomo -t".to_owned(),
                status: "exit status: 1".to_owned(),
                stderr: "rules: invalid".to_owned(),
            }
            .is_rollbackable()
        );
        assert!(
            Error::ControllerUnreachable {
                endpoint: "127.0.0.1:9090".to_owned(),
                source: "connection refused".into(),
            }
            .is_rollbackable()
        );
        assert!(Error::invalid("mode", "not a mode").is_rollbackable());
    }

    #[test]
    fn a_short_rendering_is_one_line_and_bounded() {
        let long = Error::Validation {
            problems: vec!["first problem".to_owned(), "second problem".to_owned()],
        };
        assert_eq!(long.short(), "configuration validation failed:");
        assert!(!long.short().contains('\n'));

        let wide = Error::InvalidValue {
            field: "reason",
            reason: "x".repeat(400),
        };
        let short = wide.short();
        assert!(short.chars().count() <= 120, "{}", short.chars().count());
        assert!(std::str::from_utf8(short.as_bytes()).is_ok());
    }
}
