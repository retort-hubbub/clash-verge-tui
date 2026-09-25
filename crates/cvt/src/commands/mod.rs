//! One module per subcommand group, plus the pieces they share.
//!
//! Every command has the same shape: gather data, build a report, emit it.
//! Reports are plain data, so the `--json` shape and the terminal rendering are
//! two views of one value and cannot drift apart.
//!
//! The one thing worth knowing before reading a command: the only method that
//! can write the profile index is [`Ctx::edit_store`], so a command that never
//! calls it is read-only.

pub mod backup;
pub mod config;
pub mod connections;
pub mod core;
pub mod doctor;
pub mod logs;
pub mod profiles;
pub mod proxies;
pub mod rules;
pub mod status;
pub mod test;
pub mod theme;

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::Result;
use cvt_core::enhance::diff::{Change, Diff};
use cvt_core::enhance::pipeline::{AppliedProfile, Outcome};
use cvt_core::mihomo::client::Client;
use cvt_core::mihomo::supervisor::CoreStatus;
use cvt_core::model::config::ConfigStats;
use cvt_core::validate::Report as ValidationReport;
use futures_util::stream::{self, StreamExt as _};
use serde::Serialize;
use serde_json::Value;

use crate::cli::{Command, NodeTestArgs};
use crate::context::Ctx;
use crate::exit::Exit;
use crate::output::{self, Fields, Output, Report, Table};

/// Run one command.
///
/// # Errors
/// Whatever the command failed with; `main` turns that into an exit code.
pub async fn dispatch(ctx: &Ctx, command: &Command) -> Result<()> {
    match command {
        Command::Status => status::run(ctx).await,
        // Routed by `main` before the home is opened; erroring rather than
        // panicking keeps `main` total.
        Command::Doctor => {
            Err(Exit::failure("internal error: doctor must run before the home is opened").into())
        }
        Command::Profiles { command } => profiles::run(ctx, command).await,
        Command::Backup { command } => backup::run(ctx, command).await,
        Command::Config { command } => config::run(ctx, command).await,
        Command::Proxies { command } => proxies::run(ctx, command).await,
        Command::Connections { command } => connections::run(ctx, command).await,
        Command::Rules { command } => rules::run(ctx, command).await,
        Command::Test { command } => test::run(ctx, command).await,
        Command::Core { command } => core::run(ctx, command).await,
        Command::Logs(args) => logs::run(ctx, args).await,
        Command::Theme => theme::run(ctx),
    }
}

/// The core's identity and process state.
///
/// Several commands report this, and each of them wants the same fields in the
/// same order, so it is gathered once here.
#[derive(Debug, Clone, Serialize)]
pub struct CoreInfo {
    /// `running`, `stopped`, `stale-pid` or `not-installed`.
    pub state: &'static str,
    /// The supervisor's one-line description.
    pub label: String,
    /// Whether a process is answering the pid file.
    pub running: bool,
    /// Process id, when one is running.
    pub pid: Option<u32>,
    /// Unix time the process started, when it is running.
    pub since: Option<i64>,
    /// Path of the binary, when one was found.
    pub binary: Option<String>,
    /// Version reported by the binary, when it could be read.
    pub version: Option<String>,
}

impl CoreInfo {
    /// Gather everything the supervisor knows, including the binary's version.
    ///
    /// The version costs one short-lived process, which is worth it: "the
    /// binary is there but will not run" is exactly the failure this report
    /// has to distinguish from "the binary is not there".
    #[must_use]
    pub fn gather(ctx: &Ctx) -> Self {
        let status = ctx.service().core_status();
        let binary = ctx.service().core_binary();
        let version = binary
            .as_deref()
            .and_then(|path| ctx.service().supervisor().version(path).ok())
            .map(|text| text.trim().to_owned());
        Self::from_parts(&status, binary, version)
    }

    /// Build from parts, so the checks in `doctor` can reuse the shape for a
    /// state they assembled themselves.
    #[must_use]
    pub fn from_parts(
        status: &CoreStatus,
        binary: Option<PathBuf>,
        version: Option<String>,
    ) -> Self {
        let (state, pid, since) = match status {
            CoreStatus::NotInstalled => ("not-installed", None, None),
            CoreStatus::Stopped => ("stopped", None, None),
            CoreStatus::Running { pid, since } => ("running", Some(*pid), Some(*since)),
            CoreStatus::StalePid { pid } => ("stale-pid", Some(*pid), None),
        };
        Self {
            state,
            label: status.label(),
            running: status.is_running(),
            pid,
            since,
            binary: binary.map(|path| path.display().to_string()),
            version,
        }
    }

    /// The `label: value` view.
    #[must_use]
    pub fn fields(&self) -> Fields {
        let mut fields = Fields::new();
        fields.push("core", &self.label);
        fields.push_opt(
            "binary",
            self.binary.clone().or_else(|| Some("not found".to_owned())),
        );
        fields.push_opt(
            "binary version",
            self.version.clone().or_else(|| Some("-".to_owned())),
        );
        fields.push_opt("pid", self.pid.map(|pid| pid.to_string()));
        fields.push_opt(
            "since",
            self.since.map(|secs| output::timestamp(Some(secs))),
        );
        fields
    }
}

impl Report for CoreInfo {
    fn schema(&self) -> &'static str {
        "cvt.core.status.v1"
    }

    fn render(&self, _out: Output) -> String {
        self.fields().render()
    }
}

/// One validation finding, in the shape `--json` prints it.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticInfo {
    /// Stable code, e.g. `E-DANGLING-POLICY`.
    pub code: &'static str,
    /// `error`, `warning` or `info`.
    pub severity: &'static str,
    /// What is wrong.
    pub message: String,
    /// Where it is, e.g. `rules[12]`.
    pub location: Option<String>,
    /// How to fix it.
    pub hint: Option<String>,
}

impl DiagnosticInfo {
    /// Every finding in a validation report, in the report's own order.
    #[must_use]
    pub fn collect(report: &ValidationReport) -> Vec<Self> {
        report
            .diagnostics
            .iter()
            .map(|d| Self {
                code: d.code,
                severity: d.severity.label(),
                message: d.message.clone(),
                location: d.location.clone(),
                hint: d.hint.clone(),
            })
            .collect()
    }

    /// One indented line per finding, plus its hint.
    #[must_use]
    pub fn render_all(diagnostics: &[Self], out: Output) -> String {
        let mut text = String::new();
        for d in diagnostics {
            let (code, tag) = match d.severity {
                "error" => ("\u{1b}[31m", "error"),
                "warning" => ("\u{1b}[33m", "warning"),
                _ => ("\u{1b}[36m", "info"),
            };
            let where_ = d
                .location
                .as_deref()
                .map_or(String::new(), |l| format!(" at {l}"));
            let _ = writeln!(
                text,
                "  {} {} {}{where_}",
                out.paint_stdout(code, tag),
                d.code,
                d.message
            );
            if let Some(hint) = &d.hint {
                let _ = writeln!(text, "      hint: {hint}");
            }
        }
        text.trim_end().to_owned()
    }
}

/// The shape of a generated document, without the document.
#[derive(Debug, Clone, Serialize)]
pub struct StatsInfo {
    /// Top-level keys.
    pub keys: usize,
    /// Proxies.
    pub proxies: usize,
    /// Proxy groups.
    pub groups: usize,
    /// Providers.
    pub providers: usize,
    /// Rules.
    pub rules: usize,
    /// `RULE-SET` references.
    pub rule_sets: usize,
    /// Rules that terminate matching (`MATCH`, `FINAL`).
    pub terminal_rules: usize,
}

impl From<&ConfigStats> for StatsInfo {
    fn from(stats: &ConfigStats) -> Self {
        Self {
            keys: stats.keys,
            proxies: stats.proxies,
            groups: stats.groups,
            providers: stats.providers,
            rules: stats.rules,
            rule_sets: stats.rule_sets,
            terminal_rules: stats.terminal_rules,
        }
    }
}

/// One profile the generator ran into, or skipped.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedInfo {
    /// Profile uid.
    pub uid: String,
    /// Display name.
    pub name: String,
    /// Profile type.
    pub kind: &'static str,
    /// What it did, or why it did nothing.
    pub note: String,
    /// Whether it was skipped.
    pub skipped: bool,
}

impl From<&AppliedProfile> for AppliedInfo {
    fn from(applied: &AppliedProfile) -> Self {
        Self {
            uid: applied.uid.clone(),
            name: applied.name.clone(),
            kind: applied.kind.as_str(),
            note: applied.note.clone(),
            skipped: applied.skipped,
        }
    }
}

/// One entry of the diff between the generated and the deployed document.
#[derive(Debug, Clone, Serialize)]
pub struct DiffEntryInfo {
    /// Dotted path, with list selectors.
    pub path: String,
    /// `added`, `removed`, `changed` or `reordered`.
    pub change: &'static str,
    /// The value before, when there was one.
    pub before: Option<Value>,
    /// The value after, when there is one.
    pub after: Option<Value>,
}

/// The whole diff.
#[derive(Debug, Clone, Serialize)]
pub struct DiffInfo {
    /// Whether the two documents are equivalent.
    pub empty: bool,
    /// One-line summary, e.g. `+3 rules, ~1 dns`.
    pub summary: String,
    /// The whole diff, one entry per line, as the library renders it.
    pub text: String,
    /// Entries that were added.
    pub added: usize,
    /// Entries that were removed.
    pub removed: usize,
    /// Entries whose value changed.
    pub changed: usize,
    /// Whether the entry limit was reached.
    pub truncated: bool,
    /// The entries themselves.
    pub entries: Vec<DiffEntryInfo>,
}

impl From<&Diff> for DiffInfo {
    fn from(diff: &Diff) -> Self {
        let (added, removed, changed) = diff.counts();
        let entries = diff
            .entries
            .iter()
            .map(|entry| {
                let (change, before, after) = match &entry.change {
                    Change::Added(value) => ("added", None, Some(value.clone())),
                    Change::Removed(value) => ("removed", Some(value.clone()), None),
                    Change::Changed { from, to } => {
                        ("changed", Some(from.clone()), Some(to.clone()))
                    }
                    Change::Reordered => ("reordered", None, None),
                };
                DiffEntryInfo {
                    path: entry.path.clone(),
                    change,
                    before,
                    after,
                }
            })
            .collect();
        Self {
            empty: diff.is_empty(),
            summary: diff.summary(),
            text: diff.to_string(),
            added,
            removed,
            changed,
            truncated: diff.truncated,
            entries,
        }
    }
}

/// Everything worth reporting about a generated configuration.
#[derive(Debug, Clone, Serialize)]
pub struct GeneratedInfo {
    /// Whether the document may be handed to the core.
    pub applicable: bool,
    /// The pipeline's one-line summary.
    pub summary: String,
    /// Shape of the document.
    pub stats: StatsInfo,
    /// Findings, worst first.
    pub diagnostics: Vec<DiagnosticInfo>,
    /// Number of errors.
    pub errors: usize,
    /// Number of warnings.
    pub warnings: usize,
    /// Number of informational notes.
    pub infos: usize,
    /// Profiles in the chain, in application order.
    pub profiles: Vec<AppliedInfo>,
    /// Non-fatal problems the pipeline noted.
    pub pipeline_warnings: Vec<String>,
    /// How the document differs from the deployed one.
    pub diff: DiffInfo,
}

impl From<&Outcome> for GeneratedInfo {
    fn from(outcome: &Outcome) -> Self {
        Self {
            applicable: outcome.is_applicable(),
            summary: outcome.summary(),
            stats: StatsInfo::from(&outcome.config.stats()),
            diagnostics: DiagnosticInfo::collect(&outcome.report),
            errors: outcome.report.errors(),
            warnings: outcome.report.warnings(),
            infos: outcome.report.infos(),
            profiles: outcome.applied.iter().map(AppliedInfo::from).collect(),
            pipeline_warnings: outcome.warnings.clone(),
            diff: DiffInfo::from(&outcome.diff),
        }
    }
}

impl GeneratedInfo {
    /// The `label: value` view every renderer starts from.
    #[must_use]
    pub fn fields(&self) -> Fields {
        let mut fields = Fields::new();
        fields.push("configuration", &self.summary);
        fields.push(
            "validation",
            format!(
                "{} error(s), {} warning(s), {} note(s)",
                self.errors, self.warnings, self.infos
            ),
        );
        fields.push("diff", &self.diff.summary);
        fields
    }

    /// The block of findings, or a line saying there are none.
    #[must_use]
    pub fn render_findings(&self, out: Output) -> String {
        if self.diagnostics.is_empty() {
            return "  no findings".to_owned();
        }
        DiagnosticInfo::render_all(&self.diagnostics, out)
    }
}

/// One node's latency result.
#[derive(Debug, Clone, Serialize)]
pub struct NodeDelay {
    /// Node name.
    pub node: String,
    /// Groups the node belongs to.
    pub groups: Vec<String>,
    /// Measured latency, when the node answered.
    pub delay_ms: Option<u16>,
    /// Whether the node answered in time.
    pub ok: bool,
    /// Why it did not, as one line.
    pub error: Option<String>,
}

/// Measure a set of nodes in parallel.
///
/// One request per node rather than the group endpoint, because the group
/// endpoint omits the nodes that failed and the reason they failed is the
/// interesting half of the answer.
pub async fn measure_nodes(
    client: &Client,
    targets: &[(String, Vec<String>)],
    url: &str,
    timeout_ms: u32,
    concurrency: usize,
) -> Vec<NodeDelay> {
    let mut rows: Vec<NodeDelay> = stream::iter(targets.iter().map(|(node, groups)| async move {
        match client.proxy_delay(node, url, timeout_ms, None).await {
            Ok(delay) => NodeDelay {
                node: node.clone(),
                groups: groups.clone(),
                delay_ms: Some(delay),
                ok: true,
                error: None,
            },
            Err(error) => NodeDelay {
                node: node.clone(),
                groups: groups.clone(),
                delay_ms: None,
                ok: false,
                error: Some(error.short()),
            },
        }
    }))
    .buffer_unordered(concurrency.max(1))
    .collect()
    .await;
    order_rows(&mut rows);
    rows
}

/// Fastest first, then by name.
///
/// The completion order of a parallel test is an implementation detail; a
/// report a human reads twice should be in the same order both times.
fn order_rows(rows: &mut [NodeDelay]) {
    rows.sort_by(|a, b| {
        a.ok.cmp(&b.ok)
            .reverse()
            .then(a.delay_ms.cmp(&b.delay_ms))
            .then(a.node.cmp(&b.node))
    });
}

/// The result of testing a set of nodes.
#[derive(Debug, Clone, Serialize)]
pub struct DelayReport {
    /// What was tested, e.g. `group PROXY` or `every group`.
    pub scope: String,
    /// The URL each node was asked for.
    pub url: String,
    /// Per-node timeout in milliseconds.
    pub timeout_ms: u32,
    /// How many nodes were tested at once.
    pub concurrency: usize,
    /// Nodes tested.
    pub tested: usize,
    /// Nodes that answered.
    pub reachable: usize,
    /// One row per node, fastest first.
    pub rows: Vec<NodeDelay>,
}

impl DelayReport {
    /// Assemble the report from the rows a test produced.
    #[must_use]
    pub fn new(
        scope: impl Into<String>,
        url: impl Into<String>,
        timeout_ms: u32,
        concurrency: usize,
        rows: Vec<NodeDelay>,
    ) -> Self {
        let reachable = rows.iter().filter(|row| row.ok).count();
        Self {
            scope: scope.into(),
            url: url.into(),
            timeout_ms,
            concurrency: concurrency.max(1),
            tested: rows.len(),
            reachable,
            rows,
        }
    }

    /// The table both renderings use.
    #[must_use]
    pub fn table(&self) -> Table {
        let mut table = Table::new(["node", "delay", "groups"]);
        for row in &self.rows {
            table.push([
                row.node.clone(),
                row.delay_ms.map_or_else(
                    || {
                        row.error
                            .clone()
                            .map_or_else(|| "timeout".to_owned(), |e| format!("failed: {e}"))
                    },
                    |ms| format!("{ms} ms"),
                ),
                output::joined(&row.groups, ", "),
            ]);
        }
        table
    }
}

impl Report for DelayReport {
    fn schema(&self) -> &'static str {
        "cvt.test.delay.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut text = String::new();
        text.push_str(&self.table().render());
        let _ = write!(
            text,
            "\n\n{} of {} node(s) answered ({} timeout, {} at a time)",
            self.reachable,
            self.tested,
            output::millis(u64::from(self.timeout_ms)),
            self.concurrency
        );
        text
    }
}

/// `proxies test` and `proxies test-all` share the payload; the shape name is
/// the only thing that tells a consumer which command produced it.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct ProxyDelayReport(pub DelayReport);

impl Report for ProxyDelayReport {
    fn schema(&self) -> &'static str {
        "cvt.proxies.test.v1"
    }

    fn render(&self, out: Output) -> String {
        self.0.render(out)
    }
}

/// Every node of every group, each listed once, with the groups it belongs to.
#[must_use]
pub fn nodes_by_group(groups: &[cvt_core::mihomo::types::ProxyView]) -> Vec<(String, Vec<String>)> {
    let mut map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for group in groups {
        for member in group.members() {
            let entry = map.entry(member.clone()).or_default();
            if !entry.contains(&group.name) {
                entry.push(group.name.clone());
            }
        }
    }
    map.into_iter().collect()
}

/// The default test URL and timeout, from settings.
#[must_use]
pub fn test_defaults(ctx: &Ctx) -> (String, u32, usize) {
    let test = &ctx.settings().test;
    (test.url.clone(), test.timeout_ms, test.concurrency.max(1))
}

/// `--url`, `--timeout` and `--concurrency`, falling back to settings.
///
/// One function for every latency command, so `proxies test` and `test delay`
/// cannot drift apart on what a missing flag means.
#[must_use]
pub fn node_options(ctx: &Ctx, args: &NodeTestArgs) -> (String, u32, usize) {
    let (url, timeout, concurrency) = test_defaults(ctx);
    (
        args.url.clone().unwrap_or(url),
        args.timeout.unwrap_or(timeout),
        args.concurrency.unwrap_or(concurrency).max(1),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cvt_core::mihomo::types::ProxyView;

    fn group(name: &str, members: &[&str]) -> ProxyView {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "type": "Selector",
            "all": members,
        }))
        .unwrap()
    }

    fn node(name: &str, groups: &[&str], delay: Option<u16>, ok: bool) -> NodeDelay {
        NodeDelay {
            node: name.to_owned(),
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
            delay_ms: delay,
            ok,
            error: if ok { None } else { Some("timeout".into()) },
        }
    }

    #[test]
    fn rows_are_ordered_fastest_first_and_failures_last() {
        let mut rows = vec![
            node("slow", &["A"], Some(900), true),
            node("dead", &["A"], None, false),
            node("fast", &["A"], Some(12), true),
            node("medium", &["A"], Some(120), true),
        ];
        order_rows(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.node.as_str()).collect();
        assert_eq!(names, vec!["fast", "medium", "slow", "dead"]);
    }

    #[test]
    fn equal_delays_fall_back_to_the_name_so_the_order_is_stable() {
        let mut rows = vec![
            node("b", &["A"], Some(10), true),
            node("a", &["A"], Some(10), true),
        ];
        order_rows(&mut rows);
        assert_eq!(rows[0].node, "a");
    }

    #[test]
    fn a_delay_report_counts_what_answered() {
        let report = DelayReport::new(
            "group A",
            "https://example.com",
            5000,
            4,
            vec![
                node("a", &["A"], Some(10), true),
                node("b", &["A"], None, false),
            ],
        );
        assert_eq!(report.tested, 2);
        assert_eq!(report.reachable, 1);
        let value = output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.test.delay.v1"));
        assert_eq!(value["rows"][1]["ok"], serde_json::json!(false));
        assert!(
            report
                .render(Output::new(false, 0, false))
                .contains("1 of 2")
        );
    }

    #[test]
    fn the_proxy_wrapper_renames_the_shape_without_changing_the_payload() {
        let report = DelayReport::new("group A", "u", 1, 1, vec![node("a", &["A"], Some(1), true)]);
        let wrapped = ProxyDelayReport(report);
        let value = output::to_value(&wrapped).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.proxies.test.v1"));
        assert_eq!(value["rows"][0]["node"], serde_json::json!("a"));
    }

    #[test]
    fn a_node_appears_once_with_every_group_that_lists_it() {
        let first = group("G1", &["node", "other"]);
        let second = group("G2", &["node"]);

        let nodes = nodes_by_group(&[first, second]);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].0, "node");
        assert_eq!(nodes[0].1, vec!["G1", "G2"]);
        assert_eq!(nodes[1].0, "other");
        assert_eq!(nodes[1].1, vec!["G1"]);
    }
}
