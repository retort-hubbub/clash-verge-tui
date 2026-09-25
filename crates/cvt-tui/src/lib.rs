//! Ratatui presentation layer for `clash-verge-tui`.
//!
//! This crate owns everything that touches a terminal. It depends on
//! `cvt-core` and never the other way round, and it never spawns a process or
//! opens a socket itself: any side effect is expressed as an
//! [`app::Effect`] and performed by the binary crate, which keeps the whole
//! interface testable without a terminal.
//!
//! ```text
//! cvt (binary)  ->  cvt-tui  ->  cvt-core
//! ```

#![warn(missing_docs)]

pub mod action;
pub mod keys;
pub mod theme;

pub use action::{Action, Screen};
pub use keys::{Binding, Context, Keymap};
pub use theme::Theme;
