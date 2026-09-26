//! `test` — latency and DNS, straight through the core.
//!
//! Both subcommands go through the core's own API rather than measuring
//! anything locally: the core is the only thing that knows which outbound is
//! live, and a latency measured any other way would not describe the tunnel
//! the user actually gets.

use anyhow::Result;
use cvt_core::error::Error;
use serde::Serialize;
use serde_json::Value;

use crate::cli::{DelayArgs, DnsArgs, TestCommand};
use crate::commands::{DelayReport, measure_nodes, node_options, nodes_by_group};
use crate::context::Ctx;
use crate::output::{Output, Report};

/// Run one `test` subcommand.
///
/// # Errors
/// Whatever the test failed with.
pub async fn run(ctx: &Ctx, command: &TestCommand) -> Result<()> {
    match command {
        TestCommand::Delay(args) => delay(ctx, args).await,
        TestCommand::Dns(args) => dns(ctx, args).await,
    }
}

async fn delay(ctx: &Ctx, args: &DelayArgs) -> Result<()> {
    let (url, timeout, concurrency) = node_options(ctx, &args.node);
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
