//! Config enhancement: merging, diffing, overriding and runtime generation.

pub mod diff;
pub mod merge;
pub mod overlay;
pub mod path;
pub mod pipeline;

pub use diff::{Diff, DiffEntry, diff, diff_limited};
pub use merge::{ArrayStrategy, MergeOptions, deep_merge, expand_directives, merged};
pub use overlay::Overlay;
pub use pipeline::{AppliedProfile, Outcome, Pipeline};
