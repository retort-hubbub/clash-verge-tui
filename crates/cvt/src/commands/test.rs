//! `test` — latency and DNS, straight through the core.
//!
//! Both subcommands go through the core's own API rather than measuring
//! anything locally: the core is the only thing that knows which outbound is
//! live, and a latency measured any other way would not describe the tunnel
//! the user actually gets.

use anyhow::Result;
use cvt_core::error::Error;
use futures_util::stream::StreamExt as _;
use serde::Serialize;
use serde_json::Value;

use crate::cli::{DelayArgs, DnsArgs, TestCommand, UrlsArgs};
use crate::commands::{
    DelayReport, check_url_flag, measure_nodes, node_options, nodes_by_group, resolve_limits,
};
use crate::context::Ctx;
use crate::exit::Exit;
use crate::output::{Output, Report, Table};

/// Run one `test` subcommand.
///
/// # Errors
/// Whatever the test failed with.
pub async fn run(ctx: &Ctx, command: &TestCommand) -> Result<()> {
    match command {
        TestCommand::Delay(args) => delay(ctx, args).await,
        TestCommand::Dns(args) => dns(ctx, args).await,
        TestCommand::Urls(args) => urls(ctx, args).await,
    }
}

/// One URL's result, through one node.
#[derive(Debug, Serialize)]
pub struct UrlRow {
    /// The name the URL is configured under.
    pub target: String,
    /// The URL that was fetched.
    pub url: String,
    /// Whether the fetch came back at all.
    pub ok: bool,
    /// Round trip in milliseconds, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u32>,
    /// Why it did not, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The result of `test urls`.
#[derive(Debug, Serialize)]
pub struct UrlsReport {
    /// The node everything was measured through.
    pub node: String,
    /// Per-URL timeout in milliseconds.
    pub timeout_ms: u32,
    /// How many were measured at once.
    ///
    /// Reported for the same reason the timeout is. `--concurrency` is held to
    /// a ceiling, and a ceiling nobody can see is a number that was silently
    /// changed: the other three commands that take these flags have carried
    /// this field since they were written, and this one did not — which made
    /// the clamp visible in three reports out of four. That is the
    /// member-versus-class shape one level up, in the report rather than in
    /// the guard, and the fix for it is the same: report it everywhere.
    pub concurrency: usize,
    /// How many came back.
    pub reachable: usize,
    /// One per configured URL, in configuration order.
    pub rows: Vec<UrlRow>,
    /// The library's one-line summary.
    pub summary: String,
}

impl Report for UrlsReport {
    fn schema(&self) -> &'static str {
        "cvt.test.urls.v1"
    }

    fn render(&self, out: Output) -> String {
        let mut table = Table::new(["target", "delay", "result"]);
        for row in &self.rows {
            let (delay, result) = match (row.delay_ms, &row.error) {
                (Some(ms), _) => (format!("{ms} ms"), "ok".to_owned()),
                (None, Some(error)) => ("-".to_owned(), error.clone()),
                (None, None) => ("-".to_owned(), "no answer".to_owned()),
            };
            table.push([
                row.target.clone(),
                out.paint_stdout(if row.ok { "\u{1b}[32m" } else { "\u{1b}[31m" }, &delay),
                result,
            ]);
        }
        table.render()
    }
}

/// `test urls`: every configured URL, through one node.
///
/// The point is not a latency; it is *which* sites a node can reach. A delay
/// probe says a socket opened to one host, and the host is the same for every
/// node, so a node that cannot reach anything useful still reports a healthy
/// number.
async fn urls(ctx: &Ctx, args: &UrlsArgs) -> Result<()> {
    let targets = ctx.settings().test.urls.clone();
    if args.list || args.node.is_none() {
        if args.list {
            return ctx.out().emit(&TargetsReport {
                summary: format!("{} configured URL(s)", targets.len()),
                rows: targets,
            });
        }
        return Err(Exit::failure(
            "name a node with `--node NODE`, or list the URLs with `--list`",
        )
        .into());
    }
    if targets.is_empty() {
        return Err(
            Exit::failure("no test URLs are configured; add `test.urls` to cvt.yaml").into(),
        );
    }

    let node = args.node.clone().unwrap_or_default();
    // The same resolver every other latency command uses, so the ceilings
    // cannot reach three of four — which they did, twice.
    let (_, timeout, concurrency) = resolve_limits(ctx, &args.limits)?;
    let client = ctx.client()?;

    // A node the controller does not know is a mistake in the command, not a
    // node that reached nothing. Without this the report said `reachable: 0`
    // for `--node no-such-node`, which is exactly what a node that exists and
    // cannot reach anything looks like — the two are worth telling apart, and
    // only one of them is worth retrying.
    //
    // Looked for in both places a name can be: the controller's own map, and
    // the member lists of its groups. A real core lists every proxy at the top
    // level; a minimal controller may list only groups and their members, and
    // refusing a node for not being in the first place would be this program
    // deciding a name does not exist because it asked the wrong question.
    let known = client.proxies().await?;
    let named = known.proxies.contains_key(&node)
        || known
            .proxies
            .values()
            .any(|proxy| proxy.members().iter().any(|member| member == &node));
    if !named {
        // The report is still emitted, with every row carrying the reason, and
        // the exit code is what says it failed. A `--json` consumer parses one
        // shape whatever happens; printing nothing and exiting 1 makes the
        // machine-readable mode the one that cannot be read.
        let reason = format!("no node named `{node}`");
        ctx.out().emit(&UrlsReport {
            node: node.clone(),
            timeout_ms: timeout,
            concurrency,
            reachable: 0,
            rows: targets
                .into_iter()
                .map(|target| UrlRow {
                    target: target.name,
                    url: target.url,
                    ok: false,
                    delay_ms: None,
                    error: Some(reason.clone()),
                })
                .collect(),
            summary: reason.clone(),
        })?;
        return Err(Exit::failure(reason).into());
    }

    // Measured concurrently, reported in configuration order. `buffer_unordered`
    // yields as each finishes, so the rows came back in whatever order the
    // measurements happened to complete — a report whose rows move between runs
    // is a report nobody can diff, and the order the user wrote the list in is
    // the only one with meaning.
    let measured: Vec<(usize, UrlRow)> =
        futures_util::stream::iter(targets.into_iter().enumerate().map(|(index, target)| {
            let client = client.clone();
            let node = node.clone();
            async move {
                let row = match client.proxy_delay(&node, &target.url, timeout, None).await {
                    Ok(delay_ms) => UrlRow {
                        target: target.name,
                        url: target.url,
                        ok: true,
                        delay_ms: Some(u32::from(delay_ms)),
                        error: None,
                    },
                    Err(error) => UrlRow {
                        target: target.name,
                        url: target.url,
                        ok: false,
                        delay_ms: None,
                        error: Some(error.short()),
                    },
                };
                (index, row)
            }
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await;
    let mut measured = measured;
    measured.sort_by_key(|(index, _)| *index);
    let rows: Vec<UrlRow> = measured.into_iter().map(|(_, row)| row).collect();

    let reachable = rows.iter().filter(|row| row.ok).count();
    let summary = format!(
        "{reachable} of {} URL(s) reached through {node}",
        rows.len()
    );
    ctx.out().emit(&UrlsReport {
        node,
        timeout_ms: timeout,
        concurrency,
        reachable,
        rows,
        summary,
    })
}

/// The result of `test urls --list`.
#[derive(Debug, Serialize)]
pub struct TargetsReport {
    /// Every configured URL, in configuration order.
    pub rows: Vec<cvt_core::settings::TestTarget>,
    /// The library's one-line summary.
    pub summary: String,
}

impl Report for TargetsReport {
    fn schema(&self) -> &'static str {
        "cvt.test.targets.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut table = Table::new(["name", "url"]);
        for row in &self.rows {
            table.push([row.name.clone(), row.url.clone()]);
        }
        table.render()
    }
}

async fn delay(ctx: &Ctx, args: &DelayArgs) -> Result<()> {
    check_url_flag(ctx, args.node.url.as_deref())?;
    let (url, timeout, concurrency) = node_options(ctx, &args.node)?;
    let client = ctx.client()?;

    let (scope, targets) = if args.all {
        let groups = client.groups().await?;
        let targets = nodes_by_group(&groups);
        (format!("{} group(s)", groups.len()), targets)
    } else {
        let group = args.group.clone().unwrap_or_default();
        let proxies = client.proxies().await?;
        let target = proxies
            .proxies
            .get(&group)
            .ok_or_else(|| Error::invalid("group", format!("no group named `{group}`")))?;
        let targets: Vec<(String, Vec<String>)> = target
            .members()
            .iter()
            .map(|member| (member.clone(), vec![group.clone()]))
            .collect();
        (format!("group {group}"), targets)
    };

    if targets.is_empty() {
        return Err(Error::invalid("group", "nothing to test: the selection has no nodes").into());
    }
    let rows = measure_nodes(&client, &targets, &url, timeout, concurrency).await;
    ctx.out()
        .emit(&DelayReport::new(scope, url, timeout, concurrency, rows))
}

/// The result of `test dns`.
#[derive(Debug, Serialize)]
pub struct DnsReport {
    /// The name that was resolved.
    pub name: String,
    /// The record type that was asked for.
    pub record_type: String,
    /// The core's answer, verbatim.
    pub answer: Value,
}

impl Report for DnsReport {
    fn schema(&self) -> &'static str {
        "cvt.test.dns.v1"
    }

    fn render(&self, _out: Output) -> String {
        let body =
            serde_json::to_string_pretty(&self.answer).unwrap_or_else(|_| self.answer.to_string());
        format!("{} {}:\n{body}", self.record_type, self.name)
    }
}

async fn dns(ctx: &Ctx, args: &DnsArgs) -> Result<()> {
    let client = ctx.client()?;
    let answer = client.dns_query(&args.name, &args.record_type).await?;
    ctx.out().emit(&DnsReport {
        name: args.name.clone(),
        record_type: args.record_type.to_uppercase(),
        answer,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_dns_answer_is_passed_through_untouched() {
        let report = DnsReport {
            name: "example.com".into(),
            record_type: "A".into(),
            answer: serde_json::json!({"Status": 0, "Answer": [{"data": "1.2.3.4"}]}),
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.starts_with("A example.com:"), "{text}");
        assert!(text.contains("1.2.3.4"), "{text}");
        let value = crate::output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.test.dns.v1"));
        assert_eq!(value["answer"]["Status"], serde_json::json!(0));
    }
}
