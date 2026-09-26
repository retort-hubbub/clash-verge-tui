//! Where a command's result goes.
//!
//! Two rules make the output usable from a script:
//!
//! 1. **stdout carries the result and nothing else.** Progress, hints and
//!    warnings go to stderr in both modes, so `cvt ... --json | jq` never has
//!    to filter a chatty diagnostic out of the payload.
//! 2. **The `--json` shape is a value, not a string.** Every command builds a
//!    [`Report`] and hands it to [`Output::emit`]; the JSON form is
//!    `serde`'s view of that value and the terminal form is
//!    [`Report::render`]. The two cannot drift, and a test can assert on the
//!    serialised shape without parsing prose.
//!
//! Write errors are ignored on purpose: `cvt status | head -1` closes the pipe
//! early, and that is not a failure.

use std::fmt::{self, Display, Write as _};
use std::io::{IsTerminal as _, Write as _};

use anyhow::{Context as _, Result};
use serde::Serialize;
use serde_json::{Map, Value};
use unicode_width::UnicodeWidthStr;

/// A command's result, in both of its renderings.
pub trait Report: Serialize {
    /// Stable name of this shape, emitted as the JSON `schema` field.
    ///
    /// A consumer uses it to tell shapes apart; it changes only when the shape
    /// does, and then the trailing `v1` changes with it.
    fn schema(&self) -> &'static str;

    /// The terminal rendering. Ignored when `--json` is in effect.
    ///
    /// Takes the output channel so a report can colour the parts that need it
    /// the same way everything else is coloured.
    fn render(&self, out: Output) -> String;
}

/// Render a report as the object `--json` prints.
///
/// `schema` is inserted first so that a reader sees which shape it is looking
/// at before anything else.
///
/// # Errors
/// Propagates a serialisation failure, which for these types means a bug.
pub fn to_value<R: Report + ?Sized>(report: &R) -> Result<Value> {
    let value = serde_json::to_value(report).context("could not serialise the report")?;
    let mut out = Map::new();
    out.insert(
        "schema".to_owned(),
        Value::String(report.schema().to_owned()),
    );
    match value {
        Value::Object(fields) => out.extend(fields),
        other => {
            out.insert("value".to_owned(), other);
        }
    }
    Ok(Value::Object(out))
}

/// The channel commands write to, and the switches they obey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    json: bool,
    verbose: u8,
    color: bool,
}

impl Output {
    /// Build an output channel.
    ///
    /// `color` is the *choice*; whether colour is actually emitted also
    /// depends on the stream being a terminal, which is decided per write.
    #[must_use]
    pub const fn new(json: bool, verbose: u8, color: bool) -> Self {
        Self {
            json,
            verbose,
            color,
        }
    }

    /// Whether the caller asked for machine-readable output.
    #[must_use]
    pub const fn is_json(self) -> bool {
        self.json
    }

    /// A progress or explanatory line, always on stderr.
    ///
    /// Dimmed rather than prefixed: a note is not a warning, and a report that
    /// is piped somewhere should still read as the report.
    pub fn note(self, message: impl Display) {
        write_line_to_stderr(&self.paint_stderr("\u{1b}[2m", &message.to_string()));
    }

    /// A line shown only when `-v` has been repeated at least `level` times.
    pub fn verbose(self, level: u8, message: impl Display) {
        if self.verbose >= level {
            write_line_to_stderr(&format!(
                "{} {}",
                self.paint_stderr("\u{1b}[2m", "note"),
                message
            ));
        }
    }

    /// A problem the command worked around, or that the user should know
    /// about. Still stderr: a warning is not a result.
    pub fn warn(self, message: impl Display) {
        write_line_to_stderr(&format!(
            "{} {message}",
            self.paint_stderr("\u{1b}[33m", "warning:")
        ));
    }

    /// Print a report on stdout in the shape the caller asked for.
    ///
    /// # Errors
    /// Only when the report cannot be serialised.
    pub fn emit<R: Report + ?Sized>(self, report: &R) -> Result<()> {
        let text = if self.json {
            let value = to_value(report)?;
            serde_json::to_string_pretty(&value).context("could not serialise the report")?
        } else {
            report.render(self)
        };
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{text}");
        Ok(())
    }

    /// Wrap `text` in an ANSI escape when colour is on and `stream` is a
    /// terminal.
    fn paint(self, stream: bool, code: &str, text: &str) -> String {
        if self.color && stream {
            format!("{code}{text}\u{1b}[0m")
        } else {
            text.to_owned()
        }
    }

    /// Colour for a string destined for stdout.
    #[must_use]
    pub fn paint_stdout(self, code: &str, text: &str) -> String {
        self.paint(std::io::stdout().is_terminal(), code, text)
    }

    /// Colour for a string destined for stderr.
    #[must_use]
    pub fn paint_stderr(self, code: &str, text: &str) -> String {
        self.paint(std::io::stderr().is_terminal(), code, text)
    }
}

/// One line on stdout, for the commands whose output is a stream rather than a
/// report.
pub fn line(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{text}");
}

fn write_line_to_stderr(text: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "{text}");
}

/// How a `doctor` check ended.
///
/// There is no `Skipped`: a check that could not run is a [`CheckStatus::Warn`]
/// with the reason in its detail, because "I could not tell" is exactly the
/// thing a user needs to see rather than a silently absent line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// The check ran and the result was good.
    Pass,
    /// The check ran and found something worth acting on, or could not run.
    Warn,
    /// The check ran and the result is broken.
    Fail,
}

impl CheckStatus {
    /// The lower-case word used in the report and in `--json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

impl Serialize for CheckStatus {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A `label: value` block, aligned on the widest label.
///
/// Most commands report a handful of scalars, and a table of one column of
/// labels and one of values reads better than a two-column table with a
/// meaningless header.
#[derive(Debug, Default)]
pub struct Fields {
    rows: Vec<(String, String)>,
}

impl Fields {
    /// An empty block.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a row.
    pub fn push(&mut self, label: impl Into<String>, value: impl Into<String>) {
        self.rows.push((label.into(), value.into()));
    }

    /// Add a row only when there is something to say.
    pub fn push_opt(&mut self, label: impl Into<String>, value: Option<String>) {
        if let Some(value) = value {
            self.push(label, value);
        }
    }

    /// Append every row of another block, for composing reports out of parts.
    pub fn append(&mut self, other: &Self) {
        self.rows.extend(other.rows.iter().cloned());
    }

    /// Render, aligned.
    #[must_use]
    pub fn render(&self) -> String {
        let width = self
            .rows
            .iter()
            .map(|(label, _)| UnicodeWidthStr::width(label.as_str()))
            .max()
            .unwrap_or(0);
        let mut out = String::new();
        for (label, value) in &self.rows {
            let pad = width.saturating_sub(UnicodeWidthStr::width(label.as_str()));
            let _ = writeln!(out, "{label}{}  {value}", " ".repeat(pad));
        }
        out.trim_end().to_owned()
    }
}

/// A plain-text table whose columns are sized to their contents.
#[derive(Debug, Default)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// A table with the given headers.
    #[must_use]
    pub fn new<S: Into<String>>(headers: impl IntoIterator<Item = S>) -> Self {
        Self {
            headers: headers.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
        }
    }

    /// Add a row. A short row is padded, a long one is truncated to the
    /// header count, so a caller cannot accidentally shift a column.
    pub fn push<S: Into<String>>(&mut self, row: impl IntoIterator<Item = S>) {
        let mut cells: Vec<String> = row.into_iter().map(Into::into).collect();
        cells.truncate(self.headers.len().max(1));
        while cells.len() < self.headers.len() {
            cells.push(String::new());
        }
        self.rows.push(cells);
    }

    /// Render with two spaces between columns and no trailing padding.
    #[must_use]
    pub fn render(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }
        let columns = self.headers.len().max(1);
        let mut widths = vec![0usize; columns];
        for (index, header) in self.headers.iter().enumerate() {
            widths[index] = UnicodeWidthStr::width(header.as_str());
        }
        for row in &self.rows {
            for (index, cell) in row.iter().enumerate() {
                if index < columns {
                    widths[index] = widths[index].max(UnicodeWidthStr::width(cell.as_str()));
                }
            }
        }
        let mut out = String::new();
        if !self.headers.is_empty() {
            let _ = writeln!(out, "{}", join_row(&self.headers, &widths, columns));
        }
        for row in &self.rows {
            let _ = writeln!(out, "{}", join_row(row, &widths, columns));
        }
        out.trim_end().to_owned()
    }
}

fn join_row(cells: &[String], widths: &[usize], columns: usize) -> String {
    let mut out = String::new();
    for (index, width) in widths.iter().enumerate().take(columns) {
        let cell = cells.get(index).map_or("", String::as_str);
        if index + 1 == columns {
            out.push_str(cell);
            break;
        }
        let pad = width.saturating_sub(UnicodeWidthStr::width(cell));
        out.push_str(cell);
        out.push_str(&" ".repeat(pad + 2));
    }
    out
}

/// A byte count a human can read.
#[must_use]
pub fn bytes(count: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GiB", 1024 * 1024 * 1024),
        ("MiB", 1024 * 1024),
        ("KiB", 1024),
        ("B", 1),
    ];
    for (unit, scale) in UNITS {
        if count >= scale {
            return if scale == 1 {
                format!("{count} {unit}")
            } else {
                let tenths = count.saturating_mul(10) / scale;
                format!("{}.{} {unit}", tenths / 10, tenths % 10)
            };
        }
    }
    "0 B".to_owned()
}

/// A millisecond count, switching to seconds once it stops being readable.
#[must_use]
pub fn millis(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        let tenths = ms / 100;
        format!("{}.{} s", tenths / 10, tenths % 10)
    }
}

/// A latency cell: the delay, or a dash when the node was never measured.
#[must_use]
pub fn delay(ms: Option<u16>) -> String {
    ms.map_or_else(|| "-".to_owned(), |ms| format!("{ms} ms"))
}

/// An age, relative to now: `12s ago`, `3h ago`, or `in 2d` for the future.
///
/// Relative rather than absolute because every timestamp this program shows is
/// used to answer "is this stale?", and a relative answer is the one a human
/// reads without doing arithmetic.
#[must_use]
pub fn age(seconds: i64) -> String {
    let magnitude = seconds.unsigned_abs();
    let (value, unit) = match magnitude {
        0..=59 => (magnitude, "s"),
        60..=3599 => (magnitude / 60, "m"),
        3600..=86_399 => (magnitude / 3600, "h"),
        _ => (magnitude / 86_400, "d"),
    };
    if seconds >= 0 {
        format!("{value}{unit} ago")
    } else {
        format!("in {value}{unit}")
    }
}

/// A unix timestamp as an age, or a dash when it is unknown.
#[must_use]
pub fn timestamp(seconds: Option<i64>) -> String {
    match seconds {
        None | Some(0) => "-".to_owned(),
        Some(secs) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            age(now.saturating_sub(secs))
        }
    }
}

/// Shorten `text` to `max` characters, marking the cut with an ellipsis.
///
/// A table cell is the wrong place for a 120-character error message: the
/// first line identifies the failure, and the full text is in the `--json`
/// form for anything that needs it.
#[must_use]
pub fn ellipsis(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('\u{2026}');
    out
}

/// A boolean as a compact word.
#[must_use]
pub fn yes_no(value: bool) -> String {
    if value { "yes" } else { "no" }.to_owned()
}

/// Join parts with a separator, or a dash when there is nothing to join.
#[must_use]
pub fn joined(parts: &[String], separator: &str) -> String {
    if parts.is_empty() {
        "-".to_owned()
    } else {
        parts.join(separator)
    }
}

impl fmt::Display for Fields {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Tiny {
        name: String,
        count: u64,
    }

    impl Report for Tiny {
        fn schema(&self) -> &'static str {
            "cvt.tiny.v1"
        }
        fn render(&self, _out: Output) -> String {
            format!("{}={}", self.name, self.count)
        }
    }

    fn tiny() -> Tiny {
        Tiny {
            name: "x".into(),
            count: 3,
        }
    }

    #[test]
    fn json_carries_the_schema_first_and_the_fields_after() {
        let value = to_value(&tiny()).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.tiny.v1"));
        assert_eq!(value["name"], serde_json::json!("x"));
        assert_eq!(value["count"], serde_json::json!(3));
        let keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys.first().map(String::as_str),
            Some("schema"),
            "a consumer should not have to search for the shape name"
        );
    }

    #[test]
    fn json_is_pretty_printed_on_stdout_only() {
        let text = serde_json::to_string_pretty(&to_value(&tiny()).unwrap()).unwrap();
        assert!(text.starts_with("{\n"), "{text}");
        assert_eq!(to_value(&tiny()).unwrap()["missing"], Value::Null);
    }

    #[test]
    fn a_report_that_is_not_an_object_is_still_wrapped() {
        struct Scalar;
        impl Serialize for Scalar {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_u8(7)
            }
        }
        impl Report for Scalar {
            fn schema(&self) -> &'static str {
                "cvt.scalar.v1"
            }
            fn render(&self, _out: Output) -> String {
                "7".to_owned()
            }
        }
        let value = to_value(&Scalar).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.scalar.v1"));
        assert_eq!(value["value"], serde_json::json!(7));
    }

    #[test]
    fn a_long_string_is_cut_at_a_character_boundary() {
        assert_eq!(ellipsis("short", 10), "short");
        assert_eq!(ellipsis("abcdefghij", 5), "abcd\u{2026}");
        assert_eq!(
            ellipsis("\u{4e2d}\u{6587}\u{4e2d}\u{6587}", 3),
            "\u{4e2d}\u{6587}\u{2026}"
        );
    }

    #[test]
    fn byte_counts_switch_units_at_the_right_places() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn durations_stay_readable() {
        assert_eq!(millis(0), "0 ms");
        assert_eq!(millis(999), "999 ms");
        assert_eq!(millis(1000), "1.0 s");
        assert_eq!(millis(12_345), "12.3 s");
        assert_eq!(delay(None), "-");
        assert_eq!(delay(Some(42)), "42 ms");
    }

    #[test]
    fn ages_are_relative_in_both_directions() {
        assert_eq!(age(0), "0s ago");
        assert_eq!(age(59), "59s ago");
        assert_eq!(age(60), "1m ago");
        assert_eq!(age(3600), "1h ago");
        assert_eq!(age(90_000), "1d ago");
        assert_eq!(age(-7200), "in 2h");
        assert_eq!(timestamp(None), "-");
        assert_eq!(timestamp(Some(0)), "-", "zero means 'never', not 1970");
    }

    #[test]
    fn a_table_pads_by_display_width_not_byte_length() {
        let mut table = Table::new(["name", "value"]);
        table.push(["aa", "1"]);
        // A CJK name is twice as wide as its character count; the second column
        // must still line up.
        table.push(["名字", "2"]);
        let text = table.render();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("name"), "{text}");
        assert_eq!(lines[1], "aa    1");
        assert_eq!(lines[2], "名字  2");
    }

    #[test]
    fn an_empty_table_renders_nothing() {
        let mut table = Table::new(["a", "b"]);
        assert_eq!(table.render(), "", "nothing to show");
        table.push(["1"]);
        assert_eq!(table.render().lines().count(), 2, "header plus one row");
    }

    #[test]
    fn fields_align_on_the_widest_label() {
        let mut fields = Fields::new();
        fields.push("a", "1");
        fields.push("longer", "2");
        assert_eq!(fields.render(), "a       1\nlonger  2");
        assert_eq!(Fields::new().render(), "");
    }

    #[test]
    fn option_and_join_helpers_say_something_useful() {
        assert_eq!(yes_no(true), "yes");
        assert_eq!(yes_no(false), "no");
        assert_eq!(joined(&[], ", "), "-");
        assert_eq!(joined(&["a".into(), "b".into()], " > "), "a > b");
        let mut fields = Fields::new();
        fields.push_opt("x", None);
        assert_eq!(fields.render(), "", "an absent value is not a row");
    }

    #[test]
    fn check_status_words_are_the_ones_in_the_documentation() {
        assert_eq!(CheckStatus::Pass.as_str(), "pass");
        assert_eq!(CheckStatus::Warn.as_str(), "warn");
        assert_eq!(CheckStatus::Fail.as_str(), "fail");
        assert_eq!(
            serde_json::to_value(CheckStatus::Fail).unwrap(),
            serde_json::json!("fail")
        );
    }

    #[test]
    fn colour_is_a_choice_not_a_guess() {
        let plain = Output::new(false, 0, false);
        assert_eq!(plain.paint_stdout("\u{1b}[31m", "x"), "x");
        let forced = Output::new(false, 0, true);
        // Not a terminal under `cargo test`, so still plain — but the switch
        // itself must survive the round trip.
        assert!(!forced.is_json());
        assert!(forced.paint_stderr("\u{1b}[31m", "x").contains('x'));
        assert!(Output::new(true, 2, false).is_json());
    }
}
