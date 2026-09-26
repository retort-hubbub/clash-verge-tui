//! Ratatui presentation layer for `clash-verge-tui`.
//!
//! This crate owns everything that touches a terminal, including the run loop
//! in [`run`], which takes the screen over and gives it back. It depends on
//! `cvt-core` and never the other way round, and it performs no *application*
//! I/O: it never spawns a process or opens a socket, and any side effect the
//! interface wants is expressed as [`app::Effect`] and performed by the binary
//! crate. That is what keeps the interface testable without a terminal — the
//! loop is the only part that needs one, and it takes its input stream and its
//! effect executor as arguments.
//!
//! ```text
//! cvt (binary)  ->  cvt-tui  ->  cvt-core
//! ```

#![warn(missing_docs)]

pub mod action;
pub mod app;
pub mod i18n;
pub mod keys;
pub mod row;
pub mod run;
pub mod state;
pub mod theme;
pub mod ui;

pub use action::{Action, Screen};
pub use app::{
    App, ConnectionSort, Data, Done, Effect, Event, Overlay, Preview, PromptKind, SettingKind,
    SettingRow, Status, StatusKind,
};
pub use keys::{Binding, Context, Keymap};
pub use row::{
    ConnectionRow, Live, LogRow, NodeRow, ProfileRow, RuleRow, TestKind, TestResult, TestRow,
};
pub use run::{EffectFuture, EventSink, RunError};
pub use state::{
    Filterable, LogBuffer, Metrics, SortOrder, Table, human_age, human_bytes, human_delay,
    human_rate,
};
pub use theme::Theme;
