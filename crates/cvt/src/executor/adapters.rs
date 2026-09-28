//! Adapters adapter for TUI effects.

use cvt_core::Error;
use cvt_core::enhance::pipeline::Outcome;
use cvt_core::validate::Severity;
use cvt_tui::app::{Preview, PreviewChange, PreviewFinding};

/// Summarise a generated document for the preview screen.
pub(super) fn preview_of(outcome: &Outcome) -> Preview {
    Preview {
        summary: outcome.summary(),
        changes: outcome
            .diff
            .entries
            .iter()
            .map(|entry| PreviewChange {
                verb: entry.change.verb().chars().next().unwrap_or('?'),
                path: entry.path.clone(),
            })
            .collect(),
        findings: outcome
            .report
            .diagnostics
            .iter()
            .map(|diagnostic| PreviewFinding {
                tag: match diagnostic.severity {
                    Severity::Error => 'E',
                    Severity::Warning => 'W',
                    Severity::Info => 'I',
                },
                message: diagnostic.message.clone(),
                location: diagnostic.location.clone(),
                hint: diagnostic.hint.clone(),
            })
            .collect(),
        warnings: outcome.warnings.clone(),
        applicable: outcome.is_applicable(),
        truncated: outcome.diff.truncated,
    }
}

/// Open `$EDITOR` on a path, waiting for it to exit.
pub(super) fn open_editor(path: &std::path::Path) -> Result<(), Error> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());

    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .map_err(|e| Error::io(path, e))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::ProcessFailed {
            program: editor,
            status: status.to_string(),
            stderr: String::new(),
        })
    }
}
