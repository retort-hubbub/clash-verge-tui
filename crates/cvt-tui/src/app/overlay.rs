//! Overlay responsibilities of the application state machine.

use super::STATUS_TTL;
use crate::action::Action;
use std::time::Instant;

// ---------------------------------------------------------------------------
// overlays and status
// ---------------------------------------------------------------------------

/// How bad a status message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// Neutral information.
    Info,
    /// Something worked.
    Success,
    /// Something was refused, or worked only partly.
    Warning,
    /// Something failed.
    Error,
}

/// One line of feedback for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// How bad it is.
    pub kind: StatusKind,
    /// What to say.
    pub text: String,
    /// When it was raised.
    pub at: Instant,
}

impl Status {
    /// A message raised now.
    #[must_use]
    pub fn new(kind: StatusKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            at: Instant::now(),
        }
    }

    /// Whether this message should be hidden at `now`.
    ///
    /// Transient messages carry a timestamp so that a burst of them cannot
    /// pile up: the newest replaces the previous one and goes away by itself.
    /// The complete message is retained separately after the footer expires.
    #[must_use]
    pub fn is_expired_at(&self, now: Instant) -> bool {
        now.duration_since(self.at) >= STATUS_TTL
    }
}

/// What a [`Overlay::Prompt`] is collecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// A filter for the current list, applied as it is typed.
    Search,
    /// A new name for the highlighted profile.
    Rename,
    /// A subscription URL.
    Url,
    /// A name for a new, blank profile.
    Name,
    /// A new value for the highlighted setting.
    Text,
    /// Replacement URL for the selected subscription.
    ProfileUrl,
    /// A full Mihomo rule for the active profile.
    Rule,
}

impl PromptKind {
    /// The default label, used when nothing more specific is known.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Search => "filter",
            Self::Rename => "rename profile",
            Self::Url => "subscription URL",
            Self::Name => "profile name",
            Self::Text => "value",
            Self::ProfileUrl => "subscription URL",
            Self::Rule => "new rule (TYPE,payload,policy)",
        }
    }
}

/// A modal layer that takes keys before the key map sees them.
///
/// This is what makes `q` type a `q` into a prompt instead of quitting: the
/// dispatcher never runs while an overlay is open.
#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    /// A single-line text input.
    Prompt {
        /// What is being asked for.
        label: String,
        /// What the answer is used for.
        kind: PromptKind,
        /// The text so far.
        value: String,
        /// Caret position, counted in characters rather than bytes.
        cursor: usize,
    },
    /// A yes/no question about a destructive action.
    Confirm {
        /// What the user is about to do.
        question: String,
        /// The action to run on an explicit yes.
        action: Action,
    },
    /// A list of choices.
    Picker {
        /// What is being chosen.
        title: String,
        /// The choices.
        items: Vec<String>,
        /// Which one is highlighted.
        selected: usize,
    },
    /// Scrolling text, used for the configuration preview.
    Preview {
        /// What is being shown.
        title: String,
        /// The text, one entry per line.
        lines: Vec<String>,
        /// First line on screen.
        scroll: usize,
    },
    /// A modal displaying a full status or error message.
    Message {
        /// Popup title.
        title: String,
        /// Full message text.
        text: String,
        /// Message severity.
        kind: StatusKind,
        /// First rendered row on screen.
        scroll: usize,
    },
}

impl Overlay {
    /// A one-line description, for the frame title.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::Prompt { label, .. } => label.clone(),
            Self::Confirm { question, .. } => question.clone(),
            Self::Picker { title, .. }
            | Self::Preview { title, .. }
            | Self::Message { title, .. } => title.clone(),
        }
    }
}
