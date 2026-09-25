//! Core domain logic for `clash-verge-tui`.
//!
//! This crate deliberately has **no UI dependency**. Everything here is usable
//! from a script, a test, or a future GUI front-end:
//!
//! * [`paths`] — where things live on disk,
//! * [`model`] — the config document, proxies, groups and rules,
//! * [`profile`] — the subscription store and its update pipeline,
//! * [`enhance`] — merge/override profiles and the config generation pipeline,
//! * [`mihomo`] — the core API client, process supervisor and log handling,
//! * [`validate`] — pre-flight checks run before a config reaches the core.
//!
//! The layering is strict and one-directional:
//!
//! ```text
//! cvt (binary)  ->  cvt-tui  ->  cvt-core
//! ```
//!
//! `cvt-core` never depends on either of the other two.

#![warn(missing_docs)]

pub mod enhance;
pub mod error;
pub mod model;
pub mod paths;
pub mod validate;

pub use error::{Error, Result};
pub use paths::AppPaths;
