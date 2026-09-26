//! `proxies` — policy groups and the nodes behind them.
//!
//! Listing a group expands its members and shows each one's last known delay,
//! because "which node is selected" and "which node is actually fast" are the
//! two questions this command exists to answer.

use anyhow::Result;
use cvt_core::error::Error;
use cvt_core::mihomo::types::ProxyView;
use serde::Serialize;

use crate::cli::{NodeTestArgs, ProxiesCommand, TestArgs};
use crate::commands::{DelayReport, ProxyDelayReport, measure_nodes, node_options, nodes_by_group};
use crate::context::Ctx;
use crate::output::{self, Output, Report, Table};

/// Run one `proxies` subcommand.
///
/// # Errors
/// Whatever the operation failed with.
pub async fn run(ctx: &Ctx, command: &ProxiesCommand) -> Result<()> {
    match command {
        ProxiesCommand::List { group } => list(ctx, group.as_deref()).await,
        ProxiesCommand::Select { group, node } => select(ctx, group, node).await,
        ProxiesCommand::Test(args) => test(ctx, args).await,
        ProxiesCommand::TestAll(args) => test_all(ctx, args).await,
        ProxiesCommand::Unpin { group } => unpin(ctx, group).await,
    }
}

/// One policy group.
#[derive(Debug, Serialize)]
pub struct GroupRow {
    /// Group name.
    pub name: String,
    /// Group type, e.g. `Selector`.
    pub kind: String,
    /// The node the group is currently using.
    pub now: Option<String>,
    /// How many nodes it holds.
    pub members: usize,
    /// Whether the last measurement succeeded.
    pub alive: bool,
    /// Last known delay in milliseconds.
    pub delay_ms: Option<u16>,
}

/// One node of a group.
#[derive(Debug, Serialize)]
pub struct NodeRow {
    /// Node name.
    pub name: String,
    /// Node type, e.g. `Vmess`.
    pub kind: String,
    /// Whether the group is currently using it.
    pub selected: bool,
    /// Whether the last measurement succeeded.
    pub alive: bool,
    /// Last known delay in milliseconds.
    pub delay_ms: Option<u16>,
    /// The provider it came from, when it came from one.
    pub provider: Option<String>,
}

/// The result of `proxies list`.
#[derive(Debug, Serialize)]
pub struct ProxyListReport {
    /// The group that was expanded, when one was.
    pub group: Option<String>,
    /// Groups, when no group was named.
    pub groups: Vec<GroupRow>,
    /// Nodes of the named group.
    pub nodes: Vec<NodeRow>,
}

impl Report for ProxyListReport {
    fn schema(&self) -> &'static str {
        "cvt.proxies.list.v1"
    }

    fn render(&self, _out: Output) -> String {
        if let Some(group) = &self.group {
            let mut table = Table::new(["", "node", "type", "delay", "provider"]);
            for node in &self.nodes {
                table.push([
                    if node.selected { "*" } else { "" }.to_owned(),
                    node.name.clone(),
                    node.kind.clone(),
                    output::delay(node.delay_ms),
                    node.provider.clone().unwrap_or_else(|| "-".to_owned()),
                ]);
            }
            return format!(
                "{}\n\n{} node(s) in `{group}`; `*` is the selection",
                table.render(),
                self.nodes.len()
            );
        }
        if self.groups.is_empty() {
            return "no policy groups; is the core running with a configuration?".to_owned();
        }
        let mut table = Table::new(["group", "type", "now", "nodes", "delay"]);
        for group in &self.groups {
            table.push([
                group.name.clone(),
                group.kind.clone(),
                group.now.clone().unwrap_or_else(|| "-".to_owned()),
                group.members.to_string(),
                output::delay(group.delay_ms),
            ]);
        }
        format!(
            "{}\n\n{} group(s); `proxies list <group>` expands one",
            table.render(),
            self.groups.len()
        )
    }
}

async fn list(ctx: &Ctx, group: Option<&str>) -> Result<()> {
    let client = ctx.client()?;
    let report = match group {
        None => {
            let groups = client.groups().await?;
            ProxyListReport {
                group: None,
                groups: groups.iter().map(group_row).collect(),
                nodes: Vec::new(),
            }
        }
        Some(name) => {
            let proxies = client.proxies().await?;
            let target = proxies.proxies.get(name).ok_or_else(|| {
                Error::invalid("group", format!("no group or proxy named `{name}`"))
            })?;
            if !target.is_group() {
                return Err(Error::invalid(
                    "group",
                    format!("`{name}` is a {} node, not a group", target.kind_lower()),
                )
                .into());
            }
            let selected = target.now.clone();
            let nodes = target
                .members()
                .iter()
                .map(|member| match proxies.proxies.get(member) {
                    Some(view) => node_row(view, selected.as_deref()),
                    None => NodeRow {
                        name: member.clone(),
                        kind: "-".to_owned(),
                        selected: selected.as_deref() == Some(member.as_str()),
                        alive: false,
                        delay_ms: None,
                        provider: None,
                    },
                })
                .collect();
            ProxyListReport {
                group: Some(name.to_owned()),
                groups: Vec::new(),
                nodes,
            }
        }
    };
    ctx.out().emit(&report)
}

fn group_row(view: &ProxyView) -> GroupRow {
    GroupRow {
        name: view.name.clone(),
        kind: view.kind.clone(),
        now: view.now.clone(),
        members: view.members().len(),
        alive: view.alive,
        delay_ms: view.latest_delay_or_none(),
    }
}

fn node_row(view: &ProxyView, selected: Option<&str>) -> NodeRow {
    NodeRow {
        name: view.name.clone(),
        kind: view.kind.clone(),
        selected: selected == Some(view.name.as_str()),
        alive: view.alive,
        delay_ms: view.latest_delay_or_none(),
        provider: view.provider_name.clone(),
    }
}

/// The result of `proxies select` and `proxies unpin`.
#[derive(Debug, Serialize)]
pub struct SelectionReport {
    /// Group that was changed.
    pub group: String,
    /// Node that was pinned, absent when the selection was cleared.
    pub node: Option<String>,
    /// What the change means.
    pub detail: &'static str,
}

impl Report for SelectionReport {
    fn schema(&self) -> &'static str {
        "cvt.proxies.selection.v1"
    }

    fn render(&self, _out: Output) -> String {
        match &self.node {
            Some(node) => format!("`{}` now uses `{node}`", self.group),
            None => format!("`{}` is back on its own strategy", self.group),
        }
    }
}

async fn select(ctx: &Ctx, group: &str, node: &str) -> Result<()> {
    ctx.client()?.select(group, node).await?;
    ctx.out().emit(&SelectionReport {
        group: group.to_owned(),
        node: Some(node.to_owned()),
        detail: "the selection is pinned until it is cleared or the configuration changes",
    })
}

async fn unpin(ctx: &Ctx, group: &str) -> Result<()> {
    ctx.client()?.clear_selection(group).await?;
    ctx.out().emit(&SelectionReport {
        group: group.to_owned(),
        node: None,
        detail: "the group is free to pick by its own strategy again",
    })
}

async fn test(ctx: &Ctx, args: &TestArgs) -> Result<()> {
    let (url, timeout, concurrency) = node_options(ctx, &args.node);
    let client = ctx.client()?;
    let proxies = client.proxies().await?;
    let target = proxies
        .proxies
        .get(&args.group)
        .ok_or_else(|| Error::invalid("group", format!("no group named `{}`", args.group)))?;
    let members = target.members();
    if members.is_empty() {
        return Err(
            Error::invalid("group", format!("`{}` has no members to test", args.group)).into(),
        );
    }
    let targets: Vec<(String, Vec<String>)> = members
        .iter()
        .map(|member| (member.clone(), vec![args.group.clone()]))
        .collect();
    let rows = measure_nodes(&client, &targets, &url, timeout, concurrency).await;
    let report = DelayReport::new(
        format!("group {}", args.group),
        url,
        timeout,
        concurrency,
        rows,
    );
    ctx.out().emit(&ProxyDelayReport(report))
}

async fn test_all(ctx: &Ctx, args: &NodeTestArgs) -> Result<()> {
    let (url, timeout, concurrency) = node_options(ctx, args);
    let client = ctx.client()?;
    let groups = client.groups().await?;
    let targets = nodes_by_group(&groups);
    if targets.is_empty() {
        return Err(Error::invalid("groups", "no group has any members to test").into());
    }
    let rows = measure_nodes(&client, &targets, &url, timeout, concurrency).await;
    let report = DelayReport::new(
        format!("{} group(s)", groups.len()),
        url,
        timeout,
        concurrency,
        rows,
    );
    ctx.out().emit(&ProxyDelayReport(report))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::commands::test_defaults;
    use cvt_core::AppPaths;
    use tempfile::TempDir;

    fn ctx(dir: &TempDir) -> Ctx {
        Ctx::open(AppPaths::new(dir.path()), Output::new(false, 0, false)).unwrap()
    }

    #[test]
    fn a_group_listing_marks_the_selection() {
        let view: ProxyView = serde_json::from_value(serde_json::json!({
            "name": "JP 01",
            "type": "Vmess",
            "alive": true,
            "history": [{"time": "2024-01-01T00:00:00Z", "delay": 120}],
        }))
        .unwrap();
        let row = node_row(&view, Some("JP 01"));
        assert!(row.selected);
        assert_eq!(row.delay_ms, Some(120));
        assert_eq!(row.kind, "Vmess");
        assert_eq!(row.provider, None);
    }

    #[test]
    fn a_group_summary_counts_its_members() {
        let view: ProxyView = serde_json::from_value(serde_json::json!({
            "name": "PROXY",
            "type": "Selector",
            "now": "JP 01",
            "all": ["JP 01", "US 01"],
        }))
        .unwrap();
        let row = group_row(&view);
        assert_eq!(row.members, 2);
        assert_eq!(row.now.as_deref(), Some("JP 01"));
        assert_eq!(view.members(), ["JP 01", "US 01"]);
    }

    #[test]
    fn the_json_shape_is_stable_for_both_listings() {
        let groups = ProxyListReport {
            group: None,
            groups: vec![GroupRow {
                name: "PROXY".into(),
                kind: "Selector".into(),
                now: Some("JP 01".into()),
                members: 2,
                alive: true,
                delay_ms: Some(42),
            }],
            nodes: Vec::new(),
        };
        let value = output::to_value(&groups).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.proxies.list.v1"));
        assert_eq!(value["group"], serde_json::Value::Null);
        assert_eq!(value["groups"][0]["members"], serde_json::json!(2));
        assert!(
            groups
                .render(Output::new(false, 0, false))
                .contains("PROXY")
        );

        let nodes = ProxyListReport {
            group: Some("PROXY".into()),
            groups: Vec::new(),
            nodes: vec![NodeRow {
                name: "JP 01".into(),
                kind: "Vmess".into(),
                selected: true,
                alive: true,
                delay_ms: None,
                provider: None,
            }],
        };
        let text = nodes.render(Output::new(false, 0, false));
        assert!(text.contains('*'), "{text}");
        assert!(text.contains("1 node(s) in `PROXY`"), "{text}");
    }

    #[test]
    fn an_empty_group_list_says_what_to_check() {
        let report = ProxyListReport {
            group: None,
            groups: Vec::new(),
            nodes: Vec::new(),
        };
        assert!(
            report
                .render(Output::new(false, 0, false))
                .contains("core running")
        );
    }

    #[test]
    fn the_selection_report_reads_as_a_sentence() {
        let pinned = SelectionReport {
            group: "PROXY".into(),
            node: Some("JP 01".into()),
            detail: "x",
        };
        assert_eq!(
            pinned.render(Output::new(false, 0, false)),
            "`PROXY` now uses `JP 01`"
        );
        let cleared = SelectionReport {
            group: "PROXY".into(),
            node: None,
            detail: "x",
        };
        assert!(
            cleared
                .render(Output::new(false, 0, false))
                .contains("strategy")
        );
    }

    #[test]
    fn the_defaults_come_from_settings_when_a_flag_is_absent() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx(&dir);
        let (url, timeout, concurrency) = test_defaults(&ctx);
        assert!(url.starts_with("http"), "{url}");
        assert!(timeout > 0);
        assert!(concurrency >= 1);
    }
}
