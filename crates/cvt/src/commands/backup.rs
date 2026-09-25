//! `backup` — the profiles, the settings and the overrides, saved and restored.
//!
//! Deliberately not a copy of the whole home. What is worth keeping is what a
//! person would have to recreate by hand; the generated configuration is
//! derived and already snapshotted by the pipeline, and the logs and the core's
//! working directory are large and reproducible. `cvt config snapshots` covers
//! the first of those, and this covers the rest.
//!
//! A restore is **additive**: it writes back what the backup holds and deletes
//! nothing, so a document created after the backup stays where it is. Anything
//! a restore should not keep, a person can delete — and the alternative, having
//! this command remove files from somebody's home, is a much worse failure than
//! an extra file.

use crate::cli::BackupCommand;
use crate::context::Ctx;
use crate::exit::{Exit, ExitCode};
use crate::output::{Fields, Output, Report, Table};
use anyhow::Result;
use cvt_core::service::Backup;
use serde::Serialize;

/// What a backup operation did.
#[derive(Debug, Serialize)]
pub struct BackupInfo {
    /// `created`, `restored` or `listed`.
    pub action: &'static str,
    /// The directory the operation was about.
    pub path: String,
    /// Its name, which is the second it was taken.
    pub name: String,
    /// Unix timestamp.
    pub created: i64,
    /// Files and directories in it.
    pub items: usize,
    /// For a restore: where the state it replaced was kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety: Option<String>,
    /// The library's one-line summary.
    pub summary: String,
}

impl Report for BackupInfo {
    fn schema(&self) -> &'static str {
        "cvt.backup.v1"
    }

    fn render(&self, out: Output) -> String {
        if self.action == "listed" {
            return String::new();
        }
        let mut fields = Fields::new();
        fields.push("backup", self.path.clone());
        if let Some(safety) = &self.safety {
            fields.push("kept", safety.clone());
        }
        let _ = out;
        fields.render()
    }
}

impl From<(&'static str, Backup)> for BackupInfo {
    fn from((action, backup): (&'static str, Backup)) -> Self {
        Self {
            action,
            summary: format!("{action} {}", backup.name()),
            path: backup.path.display().to_string(),
            name: backup.name(),
            created: backup.created,
            items: backup.items,
            safety: None,
        }
    }
}

/// Run one `backup` operation.
///
/// # Errors
/// [`Exit::failure`] when the operation cannot be completed.
pub async fn run(ctx: &Ctx, command: &BackupCommand) -> Result<()> {
    match command {
        BackupCommand::Create => {
            let path = ctx.service().backup()?;
            let name = path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            ctx.out().emit(&BackupInfo {
                action: "created",
                summary: format!("saved the profiles, settings and overrides as {name}"),
                path: path.display().to_string(),
                name,
                created: 0,
                items: 0,
                safety: None,
            })?;
        }
        BackupCommand::List => {
            let backups = ctx.service().backups()?;
            if backups.is_empty() {
                ctx.out().note("no backups yet; `backup create` takes one");
                return Ok(());
            }
            // One report for the whole list, rendered as a table, so `--json`
            // and the terminal are two views of the same value rather than two
            // code paths that can disagree.
            ctx.out().emit(&BackupList {
                backups: backups
                    .into_iter()
                    .map(|backup| BackupInfo::from(("listed", backup)))
                    .collect(),
            })?;
        }
        BackupCommand::Restore { name } => {
            let backups = ctx.service().backups()?;
            let chosen = match name {
                Some(wanted) => backups
                    .iter()
                    .find(|backup| &backup.name() == wanted)
                    .ok_or_else(|| {
                        Exit::new(
                            ExitCode::Usage,
                            format!(
                                "no backup called `{wanted}`; `backup list` shows what there is"
                            ),
                        )
                    })?,
                None => backups
                    .first()
                    .ok_or_else(|| Exit::failure("there are no backups to restore from"))?,
            };
            let safety = ctx.service().restore(&chosen.path)?;
            ctx.out().emit(&BackupInfo {
                action: "restored",
                summary: format!("put {} back", chosen.name()),
                path: chosen.path.display().to_string(),
                name: chosen.name(),
                created: chosen.created,
                items: chosen.items,
                safety: Some(safety.display().to_string()),
            })?;
        }
    }
    Ok(())
}

/// The current time, in the one unit the index and this module both use.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// `backup list` as one report.
#[derive(Debug, Serialize)]
pub struct BackupList {
    /// Newest first.
    pub backups: Vec<BackupInfo>,
}

impl Report for BackupList {
    fn schema(&self) -> &'static str {
        "cvt.backup.list.v1"
    }

    fn render(&self, _out: Output) -> String {
        if self.backups.is_empty() {
            return String::new();
        }
        let mut table = Table::new(["name", "taken", "items", "path"]);
        for backup in &self.backups {
            table.push([
                backup.name.clone(),
                time_of(backup.created),
                backup.items.to_string(),
                backup.path.clone(),
            ]);
        }
        table.render()
    }
}

/// A timestamp relative to now, in the largest unit that is still exact.
///
/// "3 days ago" is what a person wants when choosing between backups; the raw
/// second is available in `--json` for anything that needs it. Written out
/// rather than pulled from a crate, because the whole operation is one
/// subtraction and a divisor.
fn time_of(stamp: i64) -> String {
    let seconds = now_unix().saturating_sub(stamp).max(0);
    match seconds {
        0 => "just now".to_owned(),
        1..=59 => format!("{seconds}s ago"),
        60..=3599 => format!("{}m ago", seconds / 60),
        3600..=86_399 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_is_rendered_as_a_distance_from_now() {
        let now = now_unix();
        assert_eq!(time_of(now), "just now");
        assert_eq!(time_of(now - 30), "30s ago");
        assert_eq!(time_of(now - 90), "1m ago");
        assert_eq!(time_of(now - 7200), "2h ago");
        assert_eq!(time_of(now - 3 * 86_400), "3d ago");
        // A clock that has gone backwards is not a negative age.
        assert_eq!(time_of(i64::MAX), "just now");
    }
}
