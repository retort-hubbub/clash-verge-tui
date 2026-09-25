//! `logs` — the core's log.
//!
//! Two sources, deliberately: without `--follow` the command reads the file the
//! supervisor captured the core's own output into, which is instant, complete
//! and works when the core is down; with `--follow` it attaches to the live
//! `/logs` stream over the controller, which is the only way to see lines as
//! they happen.
//!
//! A closed stream is not a panic: `Stream::recv` returns `None`, the loop
//! ends, and the exit status says what happened.

use anyhow::{Context as _, Result};
use cvt_core::error::Error;
use cvt_core::mihomo::stream::{Event, Options, Selection, Stream};
use cvt_core::mihomo::types::LogLevel;
use serde::Serialize;

use crate::cli::LogsArgs;
use crate::context::Ctx;
use crate::output::{self, Output, Report};

/// Run `logs`.
///
/// # Errors
/// [`Error::InvalidValue`] for an unknown level, [`Error::Io`] when the log
/// file cannot be read, and [`Error::ControllerUnreachable`] when following
/// without a controller to follow.
pub async fn run(ctx: &Ctx, args: &LogsArgs) -> Result<()> {
    let minimum = match &args.level {
        Some(level) => Some(LogLevel::parse(level).ok_or_else(|| {
            Error::invalid(
                "level",
                format!("`{level}` is not a level; use silent, error, warning, info or debug"),
            )
        })?),
        None => None,
    };

    if args.follow {
        return follow(ctx, args, minimum).await;
    }
    tail(ctx, args, minimum)
}

/// One log line.
#[derive(Debug, Serialize)]
pub struct LogEntry {
    /// The level the line declares, when it declares one.
    pub level: Option<&'static str>,
    /// The `msg=` field, or the whole line when there is none.
    pub message: String,
    /// The line exactly as the core wrote it.
    pub raw: String,
}

impl LogEntry {
    /// Parse one line of the core's logfmt output.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let fields = split_fields(raw);
        let level = fields
            .iter()
            .find(|(key, _)| *key == "level")
            .and_then(|(_, value)| LogLevel::parse(value));
        let message = fields
            .iter()
            .find(|(key, _)| *key == "msg")
            .map_or_else(|| raw.trim().to_owned(), |(_, value)| (*value).to_owned());
        Self {
            level: level.map(LogLevel::as_str),
            message,
            raw: raw.to_owned(),
        }
    }

    /// The level as an enum, for comparisons.
    fn level_value(&self) -> Option<LogLevel> {
        self.level.and_then(LogLevel::parse)
    }
}

/// The result of reading the log file.
#[derive(Debug, Serialize)]
pub struct LogTailReport {
    /// Path of the file that was read.
    pub path: String,
    /// How many lines were asked for; `0` means everything.
    pub lines_requested: usize,
    /// How many were printed after filtering.
    pub lines_returned: usize,
    /// The minimum level that was applied.
    pub level: Option<String>,
    /// The substring that was applied.
    pub filter: Option<String>,
    /// The lines, oldest first.
    pub entries: Vec<LogEntry>,
}

impl Report for LogTailReport {
    fn schema(&self) -> &'static str {
        "cvt.logs.tail.v1"
    }

    fn render(&self, _out: Output) -> String {
        if self.entries.is_empty() {
            return format!("no matching lines in {}", self.path);
        }
        self.entries
            .iter()
            .map(|entry| entry.raw.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn tail(ctx: &Ctx, args: &LogsArgs, minimum: Option<LogLevel>) -> Result<()> {
    let path = ctx.paths().core_log();
    let text = ctx.paths().read(&path).map_err(|error| {
        Error::invalid(
            "core log",
            format!(
                "{error}; the file is created when the core starts, with `clash-verge-tui core start`"
            ),
        )
    })?;
    let entries: Vec<LogEntry> = tail_lines(&text, args.lines)
        .into_iter()
        .map(LogEntry::parse)
        .filter(|entry| keeps(entry, minimum, args.filter.as_deref()))
        .collect();
    let report = LogTailReport {
        path: path.display().to_string(),
        lines_requested: args.lines,
        lines_returned: entries.len(),
        level: minimum.map(|l| l.as_str().to_owned()),
        filter: args.filter.clone(),
        entries,
    };
    ctx.out().emit(&report)
}

/// The last `count` lines, oldest first. `count == 0` means all of them.
fn tail_lines(text: &str, count: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    if count == 0 || lines.len() <= count {
        lines
    } else {
        lines[lines.len() - count..].to_vec()
    }
}

/// `true` when a line passes the level and substring filters.
///
/// `at_most` is the *most verbose* level to show. [`LogLevel`] orders quietest
/// first (`Silent < Error < Warning < Info < Debug`), so a line is kept when
/// its level is at or below the requested one — `--level warning` shows
/// warnings and errors, not info.
///
/// A line whose level cannot be determined is always kept: those are the
/// continuation lines of a multi-line message, and dropping them would cut a
/// crash report in half.
fn keeps(entry: &LogEntry, at_most: Option<LogLevel>, needle: Option<&str>) -> bool {
    let level_ok =
        at_most.is_none_or(|at_most| entry.level_value().is_none_or(|level| level <= at_most));
    let text_ok =
        needle.is_none_or(|needle| entry.raw.contains(needle) || entry.message.contains(needle));
    level_ok && text_ok
}

/// Split a logfmt line into `key=value` pairs, honouring quoted values.
///
/// A whitespace split would cut `msg="Start initial configuration"` in half,
/// and the message is the half a human reads.
fn split_fields(line: &str) -> Vec<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut fields = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index] == b' ' {
            index += 1;
        }
        let key_start = index;
        while index < bytes.len() && bytes[index] != b' ' && bytes[index] != b'=' {
            index += 1;
        }
        let key = &line[key_start..index];
        if index < bytes.len() && bytes[index] == b'=' {
            index += 1;
            if index < bytes.len() && bytes[index] == b'"' {
                index += 1;
                let value_start = index;
                while index < bytes.len() && bytes[index] != b'"' {
                    index += 1;
                }
                fields.push((key, &line[value_start..index]));
                index += 1;
            } else {
                let value_start = index;
                while index < bytes.len() && bytes[index] != b' ' {
                    index += 1;
                }
                fields.push((key, &line[value_start..index]));
            }
        } else if !key.is_empty() {
            fields.push((key, ""));
        }
    }
    fields
}

async fn follow(ctx: &Ctx, args: &LogsArgs, minimum: Option<LogLevel>) -> Result<()> {
    let endpoint = ctx
        .service()
        .endpoint()?
        .ok_or_else(|| Error::ControllerUnreachable {
            endpoint: "unknown".to_owned(),
            source: "no profile declares an `external-controller`".into(),
        })?;
    let level = minimum.unwrap_or_else(|| ctx.settings().ui.log_level);
    let mut selection = Selection::none();
    selection.logs = true;
    let options = Options::new(selection).with_log_level(level);

    // The endpoint is described in the error a never-opened stream produces,
    // so it has to outlive the stream itself.
    let description = endpoint.describe();
    let mut stream = Stream::spawn(endpoint, options).context("could not start the log stream")?;
    ctx.out().verbose(
        1,
        format!(
            "following the core's log at level {}; press Ctrl-C to stop",
            level.as_str()
        ),
    );

    let mut printed: u64 = 0;
    let mut opened = false;
    let mut never_opened: Option<String> = None;
    loop {
        tokio::select! {
            interrupt = tokio::signal::ctrl_c() => {
                // A failure to install the handler is not worth hiding the log
                // for; the loop simply keeps running.
                if interrupt.is_ok() {
                    ctx.out().note("stopped following the log");
                    break;
                }
            }
            event = stream.recv() => {
                match event {
                    None => {
                        ctx.out().note("the log stream closed");
                        break;
                    }
                    Some(Event::Log(log)) => {
                        // Built directly rather than by re-parsing a formatted
                        // line: the payload is free text and may contain the
                        // quotes a logfmt parse would look for.
                        let entry = LogEntry {
                            level: LogLevel::parse(&log.level).map(LogLevel::as_str),
                            message: log.payload.clone(),
                            raw: format!("{} {}", log.level, log.payload),
                        };
                        if keeps(&entry, None, args.filter.as_deref()) {
                            print_entry(*ctx.out(), &entry);
                            printed += 1;
                        }
                    }
                    Some(Event::Opened { name, transport }) => {
                        opened = true;
                        ctx.out().verbose(1, format!("{name} stream open over {}", transport.label()));
                    }
                    Some(Event::Closed { name, reason, retry_in }) => {
                        // A websocket handshake failure is reported with no
                        // delay and immediately retried over HTTP, so it is
                        // not yet a failure. Anything else, before the stream
                        // has ever opened, means there is no core to follow —
                        // and retrying forever would be a worse answer than
                        // the error itself.
                        if !opened && !retry_in.is_zero() {
                            never_opened = Some(reason);
                            break;
                        }
                        ctx.out().warn(format!(
                            "{name} stream closed ({reason}); retrying in {} ms",
                            retry_in.as_millis()
                        ));
                    }
                    Some(Event::Dropped { count }) => {
                        ctx.out().warn(format!("{count} log line(s) were dropped; the stream could not keep up"));
                    }
                    Some(_) => {}
                }
            }
        }
    }
    stream.shutdown().await;
    if let Some(reason) = never_opened {
        return Err(Error::ControllerUnreachable {
            endpoint: description,
            source: reason.into(),
        }
        .into());
    }
    ctx.out().verbose(1, format!("{printed} line(s) printed"));
    Ok(())
}

/// One line on stdout, in whichever shape the caller asked for.
fn print_entry(out: Output, entry: &LogEntry) {
    if out.is_json() {
        let value = serde_json::json!({
            "schema": "cvt.logs.entry.v1",
            "level": entry.level,
            "message": entry.message,
            "raw": entry.raw,
        });
        match serde_json::to_string(&value) {
            Ok(text) => output::line(&text),
            Err(_) => output::line(&entry.raw),
        }
    } else {
        output::line(&entry.raw);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_logfmt_line_is_split_without_cutting_the_message_in_half() {
        let entry = LogEntry::parse(
            r#"time="2024-01-01T00:00:00Z" level=info msg="Start initial configuration in progress""#,
        );
        assert_eq!(entry.level, Some("info"));
        assert_eq!(entry.message, "Start initial configuration in progress");
        assert!(entry.raw.contains("time="));
    }

    #[test]
    fn a_line_without_a_level_is_still_readable() {
        let entry = LogEntry::parse("panic: runtime error");
        assert_eq!(entry.level, None);
        assert_eq!(entry.message, "panic: runtime error");
    }

    #[test]
    fn a_quoted_value_containing_an_equals_sign_survives() {
        let fields = split_fields(r#"level=warning msg="dns: query=a.com failed" other=1"#);
        assert_eq!(
            fields,
            vec![
                ("level", "warning"),
                ("msg", "dns: query=a.com failed"),
                ("other", "1"),
            ]
        );
    }

    #[test]
    fn the_level_filter_keeps_only_what_was_asked_for() {
        let error = LogEntry::parse(r#"level=error msg="boom""#);
        let info = LogEntry::parse(r#"level=info msg="hello""#);
        let banner = LogEntry::parse("Mihomo Meta v1.19.31");
        assert!(keeps(&error, Some(LogLevel::Warning), None));
        assert!(!keeps(&info, Some(LogLevel::Warning), None));
        assert!(
            keeps(&banner, Some(LogLevel::Warning), None),
            "a line with no level is kept: it is usually a continuation"
        );
        assert!(keeps(&info, None, None));
    }

    #[test]
    fn the_substring_filter_looks_at_the_whole_line() {
        let entry = LogEntry::parse(r#"level=info msg="hello" tag=dns"#);
        assert!(keeps(&entry, None, Some("dns")));
        assert!(!keeps(&entry, None, Some("http")));
    }

    #[test]
    fn a_tail_returns_the_last_lines_oldest_first() {
        let text = "a\nb\nc\nd\n";
        assert_eq!(tail_lines(text, 2), vec!["c", "d"]);
        assert_eq!(tail_lines(text, 0), vec!["a", "b", "c", "d"]);
        assert_eq!(tail_lines(text, 99), vec!["a", "b", "c", "d"]);
        assert!(tail_lines("", 5).is_empty());
    }

    #[test]
    fn the_tail_report_prints_raw_lines_so_it_can_be_piped() {
        let report = LogTailReport {
            path: "/home/u/logs/core.log".into(),
            lines_requested: 200,
            lines_returned: 2,
            level: None,
            filter: None,
            entries: vec![
                LogEntry::parse(r#"level=info msg="one""#),
                LogEntry::parse(r#"level=error msg="two""#),
            ],
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.starts_with("level=info"), "{text}");
        assert_eq!(text.lines().count(), 2);
        let value = crate::output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.logs.tail.v1"));
        assert_eq!(value["entries"][1]["level"], serde_json::json!("error"));
        assert_eq!(value["entries"][1]["message"], serde_json::json!("two"));
    }

    #[test]
    fn an_empty_tail_says_where_it_looked() {
        let report = LogTailReport {
            path: "/x/core.log".into(),
            lines_requested: 10,
            lines_returned: 0,
            level: Some("error".into()),
            filter: None,
            entries: Vec::new(),
        };
        assert_eq!(
            report.render(Output::new(false, 0, false)),
            "no matching lines in /x/core.log"
        );
    }
}
