//! `connections` — what the core is carrying right now.
//!
//! Connections are the most volatile thing the API exposes: every listing is a
//! snapshot, and an id from an earlier listing may already be gone. That is why
//! `close` reports what the core said rather than assuming success.

use anyhow::Result;
use cvt_core::error::Error;
use cvt_core::mihomo::types::{Connection, ConnectionsResponse};
use serde::Serialize;

use crate::cli::{CloseArgs, ConnectionsCommand};
use crate::context::Ctx;
use crate::output::{self, Output, Report, Table};

/// Run one `connections` subcommand.
///
/// # Errors
/// Whatever the operation failed with.
pub async fn run(ctx: &Ctx, command: &ConnectionsCommand) -> Result<()> {
    match command {
        ConnectionsCommand::List { limit } => list(ctx, *limit).await,
        ConnectionsCommand::Close(args) => close(ctx, args).await,
    }
}

/// One connection, flattened to what a table can show.
#[derive(Debug, Serialize)]
pub struct ConnectionRow {
    /// Connection id.
    pub id: String,
    /// Host or IP the connection is to.
    pub destination: String,
    /// Transport, e.g. `tcp`.
    pub network: String,
    /// The rule that matched.
    pub rule: String,
    /// The proxy chain, outermost first.
    pub chains: Vec<String>,
    /// Bytes uploaded.
    pub upload: u64,
    /// Bytes downloaded.
    pub download: u64,
    /// When the connection started, as the core reports it.
    pub start: String,
    /// The process that opened it, when the core could tell.
    pub process: Option<String>,
}

impl From<&Connection> for ConnectionRow {
    fn from(connection: &Connection) -> Self {
        let meta = connection.meta();
        Self {
            id: connection.id.clone(),
            destination: meta.destination_endpoint(),
            network: meta.network.clone(),
            rule: connection.rule_label(),
            chains: connection.chains.clone(),
            upload: connection.upload,
            download: connection.download,
            start: connection.start.clone(),
            process: Some(meta.process).filter(|p| !p.is_empty()),
        }
    }
}

/// The result of `connections list`.
#[derive(Debug, Serialize)]
pub struct ConnectionListReport {
    /// Total bytes downloaded since the core started.
    pub download_total: u64,
    /// Total bytes uploaded since the core started.
    pub upload_total: u64,
    /// Memory in use, when the core reports it.
    pub memory: u64,
    /// Connections the core knows about.
    pub total: usize,
    /// How many were printed.
    pub shown: usize,
    /// The limit that was applied, when one was.
    pub limit: Option<usize>,
    /// The connections themselves.
    pub connections: Vec<ConnectionRow>,
}

impl Report for ConnectionListReport {
    fn schema(&self) -> &'static str {
        "cvt.connections.list.v1"
    }

    fn render(&self, _out: Output) -> String {
        if self.connections.is_empty() {
            return format!(
                "no live connections ({} up, {} down since the core started)",
                output::bytes(self.upload_total),
                output::bytes(self.download_total)
            );
        }
        let mut table = Table::new(["id", "destination", "net", "rule", "chain", "up", "down"]);
        for row in &self.connections {
            table.push([
                short_id(&row.id),
                row.destination.clone(),
                row.network.clone(),
                row.rule.clone(),
                output::joined(&row.chains, " > "),
                output::bytes(row.upload),
                output::bytes(row.download),
            ]);
        }
        format!(
            "{}\n\n{} of {} connection(s) shown; {} up, {} down in total",
            table.render(),
            self.shown,
            self.total,
            output::bytes(self.upload_total),
            output::bytes(self.download_total)
        )
    }
}

/// Connection ids are long UUIDs; the first group is enough to recognise one,
/// and `connections close` accepts the full id.
fn short_id(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_owned()
}

async fn list(ctx: &Ctx, limit: Option<usize>) -> Result<()> {
    let client = ctx.client()?;
    let response: ConnectionsResponse = client.connections().await?;
    let all = response.items();
    // `--limit 0` means "no limit", the same as not passing the flag.
    let shown = match limit {
        None | Some(0) => all.len(),
        Some(limit) => limit.min(all.len()),
    };
    let report = ConnectionListReport {
        download_total: response.download_total,
        upload_total: response.upload_total,
        memory: response.memory,
        total: all.len(),
        shown,
        limit,
        connections: all.iter().take(shown).map(ConnectionRow::from).collect(),
    };
    ctx.out().emit(&report)
}

/// The result of `connections close`.
#[derive(Debug, Serialize)]
pub struct CloseReport {
    /// `all`, or the id that was closed.
    pub target: String,
    /// How many connections the request covered.
    pub closed: usize,
    /// The total the core reported before the request, when it was read.
    pub before: Option<usize>,
}

impl Report for CloseReport {
    fn schema(&self) -> &'static str {
        "cvt.connections.close.v1"
    }

    fn render(&self, _out: Output) -> String {
        match self.closed {
            0 => format!("nothing to close for {target}", target = self.target),
            1 => format!("closed {}", self.target),
            n => format!("closed {n} connection(s)"),
        }
    }
}

async fn close(ctx: &Ctx, args: &CloseArgs) -> Result<()> {
    let client = ctx.client()?;
    if args.all {
        let before = client.connections().await.ok().map(|c| c.items().len());
        client.close_all_connections().await?;
        return ctx.out().emit(&CloseReport {
            target: "all".to_owned(),
            closed: before.unwrap_or(0),
            before,
        });
    }
    let Some(id) = args.id.clone() else {
        return Err(Error::invalid("id", "no connection id and no --all").into());
    };
    client.close_connection(&id).await?;
    ctx.out().emit(&CloseReport {
        target: id,
        closed: 1,
        before: None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        serde_json::from_value(serde_json::json!({
            "id": "5a3f1c2e-1111-2222-3333-444455556666",
            "upload": 1024,
            "download": 2048,
            "start": "2024-01-01T00:00:00Z",
            "chains": ["PROXY", "JP 01"],
            "rule": "RuleSet",
            "rulePayload": "reject",
            "metadata": {
                "network": "tcp",
                "host": "example.com",
                "destinationPort": "443",
                "process": "curl",
            },
        }))
        .unwrap()
    }

    #[test]
    fn a_connection_row_keeps_what_a_human_needs() {
        let row = ConnectionRow::from(&connection());
        assert_eq!(row.destination, "example.com:443");
        assert_eq!(row.chains, vec!["PROXY", "JP 01"]);
        assert_eq!(row.process.as_deref(), Some("curl"));
        assert_eq!(row.upload, 1024);
        assert_eq!(short_id(&row.id), "5a3f1c2e");
    }

    #[test]
    fn the_listing_reports_the_limit_it_applied() {
        let row = ConnectionRow::from(&connection());
        let report = ConnectionListReport {
            download_total: 4096,
            upload_total: 1024,
            memory: 0,
            total: 1,
            shown: 1,
            limit: Some(20),
            connections: vec![row],
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("1 of 1 connection(s) shown"), "{text}");
        assert!(text.contains("4.0 KiB"), "{text}");
        let value = output::to_value(&report).unwrap();
        assert_eq!(
            value["schema"],
            serde_json::json!("cvt.connections.list.v1")
        );
        assert_eq!(value["limit"], serde_json::json!(20));
        assert_eq!(
            value["connections"][0]["chains"][1],
            serde_json::json!("JP 01")
        );
    }

    #[test]
    fn an_idle_core_says_so_instead_of_printing_an_empty_table() {
        let report = ConnectionListReport {
            download_total: 0,
            upload_total: 0,
            memory: 0,
            total: 0,
            shown: 0,
            limit: None,
            connections: Vec::new(),
        };
        assert!(
            report
                .render(Output::new(false, 0, false))
                .contains("no live connections")
        );
    }

    #[test]
    fn the_close_report_counts_what_it_closed() {
        let one = CloseReport {
            target: "abc".into(),
            closed: 1,
            before: None,
        };
        assert_eq!(one.render(Output::new(false, 0, false)), "closed abc");
        let many = CloseReport {
            target: "all".into(),
            closed: 7,
            before: Some(7),
        };
        assert_eq!(
            many.render(Output::new(false, 0, false)),
            "closed 7 connection(s)"
        );
        let none = CloseReport {
            target: "all".into(),
            closed: 0,
            before: Some(0),
        };
        assert!(
            none.render(Output::new(false, 0, false))
                .contains("nothing to close")
        );
    }
}
