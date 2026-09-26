//! `rules` — the active rule set and the providers behind it.
//!
//! Rule indices are the core's own, and they are what `toggle` takes: an index
//! printed by `rules list` is the index the API expects, so the two commands
//! cannot disagree.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::Result;
use cvt_core::error::Error;
use cvt_core::mihomo::types::{RuleInfo, RuleProviderInfo};
use serde::Serialize;

use crate::cli::RulesCommand;
use crate::context::Ctx;
use crate::exit::Exit;
use crate::output::{Output, Report, Table};

/// Run one `rules` subcommand.
///
/// # Errors
/// Whatever the operation failed with.
pub async fn run(ctx: &Ctx, command: &RulesCommand) -> Result<()> {
    match command {
        RulesCommand::List { disabled, stats } => list(ctx, *disabled, *stats).await,
        RulesCommand::Toggle { index } => toggle(ctx, *index).await,
        RulesCommand::Providers => providers(ctx).await,
        RulesCommand::UpdateProviders { name } => update_providers(ctx, name.as_deref()).await,
    }
}

/// One rule.
#[derive(Debug, Serialize)]
pub struct RuleRow {
    /// The core's index for this rule.
    pub index: u32,
    /// Rule type, e.g. `DomainSuffix`.
    pub kind: String,
    /// What the rule matches.
    pub payload: String,
    /// Where a match goes.
    pub proxy: String,
    /// Whether the rule is currently disabled.
    pub disabled: bool,
    /// How many times it has matched, when the core reports statistics.
    pub hit_count: u64,
    /// How many times it has been evaluated.
    pub evaluations: u64,
}

impl From<&RuleInfo> for RuleRow {
    fn from(rule: &RuleInfo) -> Self {
        let stats = rule.stats();
        Self {
            index: rule.index,
            kind: rule.kind.clone(),
            payload: rule.payload.clone(),
            proxy: rule.proxy.clone(),
            disabled: stats.disabled,
            hit_count: stats.hit_count,
            evaluations: stats.evaluations(),
        }
    }
}

/// The result of `rules list`.
#[derive(Debug, Serialize)]
pub struct RuleListReport {
    /// Rules the core holds.
    pub total: usize,
    /// Rules that are disabled.
    pub disabled: usize,
    /// How many were printed after filtering.
    pub shown: usize,
    /// Whether the listing was filtered to disabled rules.
    pub filtered: bool,
    /// Whether hit counts were included.
    pub with_stats: bool,
    /// The rules themselves.
    pub rules: Vec<RuleRow>,
}

impl Report for RuleListReport {
    fn schema(&self) -> &'static str {
        "cvt.rules.list.v1"
    }

    fn render(&self, _out: Output) -> String {
        if self.rules.is_empty() {
            return if self.filtered {
                format!("no disabled rules out of {}", self.total)
            } else {
                "the core holds no rules".to_owned()
            };
        }
        let mut headers = vec!["#", "type", "payload", "target", "state"];
        if self.with_stats {
            headers.push("hits");
            headers.push("evaluations");
        }
        let mut table = Table::new(headers);
        for rule in &self.rules {
            let mut row = vec![
                rule.index.to_string(),
                rule.kind.clone(),
                rule.payload.clone(),
                rule.proxy.clone(),
                if rule.disabled { "disabled" } else { "on" }.to_owned(),
            ];
            if self.with_stats {
                row.push(rule.hit_count.to_string());
                row.push(rule.evaluations.to_string());
            }
            table.push(row);
        }
        format!(
            "{}\n\n{} of {} rule(s){}",
            table.render(),
            self.shown,
            self.total,
            if self.disabled > 0 {
                format!("; {} disabled", self.disabled)
            } else {
                String::new()
            }
        )
    }
}

async fn list(ctx: &Ctx, disabled_only: bool, with_stats: bool) -> Result<()> {
    let client = ctx.client()?;
    let rules = client.rules().await?;
    let rows: Vec<RuleRow> = rules
        .iter()
        .map(RuleRow::from)
        .filter(|row| !disabled_only || row.disabled)
        .collect();
    let report = RuleListReport {
        total: rules.len(),
        disabled: rules.iter().filter(|r| r.stats().disabled).count(),
        shown: rows.len(),
        filtered: disabled_only,
        with_stats,
        rules: rows,
    };
    ctx.out().emit(&report)
}

/// The result of `rules toggle`.
#[derive(Debug, Serialize)]
pub struct RuleToggleReport {
    /// The rule's index.
    pub index: u32,
    /// Rule type.
    pub kind: String,
    /// What the rule matches.
    pub payload: String,
    /// Where a match goes.
    pub proxy: String,
    /// The state after the change.
    pub disabled: bool,
}

impl Report for RuleToggleReport {
    fn schema(&self) -> &'static str {
        "cvt.rules.toggle.v1"
    }

    fn render(&self, _out: Output) -> String {
        format!(
            "rule {} ({},{} -> {}) is now {}",
            self.index,
            self.kind,
            self.payload,
            self.proxy,
            if self.disabled { "disabled" } else { "enabled" }
        )
    }
}

async fn toggle(ctx: &Ctx, index: u32) -> Result<()> {
    let client = ctx.client()?;
    let rules = client.rules().await?;
    let rule = rules
        .iter()
        .find(|rule| rule.index == index)
        .ok_or_else(|| Error::invalid("index", format!("no rule with index {index}")))?;
    let disabled = !rule.stats().disabled;

    let mut change = BTreeMap::new();
    change.insert(index, disabled);
    client.set_rules_disabled(&change).await?;

    ctx.out().emit(&RuleToggleReport {
        index: rule.index,
        kind: rule.kind.clone(),
        payload: rule.payload.clone(),
        proxy: rule.proxy.clone(),
        disabled,
    })
}

/// One rule provider.
#[derive(Debug, Serialize)]
pub struct RuleProviderRow {
    /// Provider name.
    pub name: String,
    /// `domain`, `ipcidr` or `classical`.
    pub behavior: String,
    /// `yaml`, `text` or `mrs`.
    pub format: String,
    /// How many rules it contributes.
    pub rule_count: u64,
    /// How it is fetched.
    pub vehicle: String,
    /// When it was last refreshed, as the core reports it.
    pub updated_at: Option<String>,
}

impl From<(&String, &RuleProviderInfo)> for RuleProviderRow {
    fn from((name, provider): (&String, &RuleProviderInfo)) -> Self {
        Self {
            name: name.clone(),
            behavior: provider.behavior.clone(),
            format: provider.format.clone(),
            rule_count: provider.rule_count,
            vehicle: provider.vehicle_type.clone(),
            updated_at: provider.updated_at.clone(),
        }
    }
}

/// The result of `rules providers`.
#[derive(Debug, Serialize)]
pub struct RuleProvidersReport {
    /// Providers, in name order.
    pub providers: Vec<RuleProviderRow>,
}

impl Report for RuleProvidersReport {
    fn schema(&self) -> &'static str {
        "cvt.rules.providers.v1"
    }

    fn render(&self, _out: Output) -> String {
        if self.providers.is_empty() {
            return "the core holds no rule providers".to_owned();
        }
        let mut table = Table::new(["name", "behavior", "format", "rules", "updated"]);
        for provider in &self.providers {
            table.push([
                provider.name.clone(),
                provider.behavior.clone(),
                provider.format.clone(),
                provider.rule_count.to_string(),
                provider
                    .updated_at
                    .clone()
                    .unwrap_or_else(|| "-".to_owned()),
            ]);
        }
        format!(
            "{}\n\n{} provider(s); `rules update-providers <name>` refreshes one",
            table.render(),
            self.providers.len()
        )
    }
}

async fn providers(ctx: &Ctx) -> Result<()> {
    let client = ctx.client()?;
    let response = client.rule_providers().await?;
    ctx.out().emit(&RuleProvidersReport {
        providers: response
            .providers
            .iter()
            .map(RuleProviderRow::from)
            .collect(),
    })
}

/// One provider that could not be refreshed.
#[derive(Debug, Serialize)]
pub struct ProviderFailure {
    /// Provider name.
    pub name: String,
    /// Why it failed.
    pub error: String,
}

/// The result of `rules update-providers`.
#[derive(Debug, Serialize)]
pub struct ProviderUpdateReport {
    /// Providers that were refreshed.
    pub updated: Vec<String>,
    /// Providers that could not be.
    pub failed: Vec<ProviderFailure>,
}

impl Report for ProviderUpdateReport {
    fn schema(&self) -> &'static str {
        "cvt.rules.update_providers.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut text = format!("refreshed {} provider(s)", self.updated.len());
        if !self.updated.is_empty() {
            let _ = write!(text, ": {}", self.updated.join(", "));
        }
        for failure in &self.failed {
            let _ = write!(text, "\n{}: {}", failure.name, failure.error);
        }
        text
    }
}

async fn update_providers(ctx: &Ctx, name: Option<&str>) -> Result<()> {
    let client = ctx.client()?;
    let names: Vec<String> = match name {
        Some(name) => vec![name.to_owned()],
        None => client
            .rule_providers()
            .await?
            .providers
            .keys()
            .cloned()
            .collect(),
    };
    if names.is_empty() {
        ctx.out()
            .note("the core holds no rule providers to refresh");
    }

    let mut updated = Vec::new();
    let mut failed = Vec::new();
    for name in names {
        match client.update_rule_provider(&name).await {
            Ok(()) => updated.push(name),
            Err(error) => failed.push(ProviderFailure {
                name,
                error: error.short(),
            }),
        }
    }
    let report = ProviderUpdateReport { updated, failed };
    let failures = report.failed.len();
    ctx.out().emit(&report)?;
    if failures > 0 {
        return Err(Exit::failure(format!("{failures} provider(s) could not be refreshed")).into());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn rule() -> RuleInfo {
        serde_json::from_value(serde_json::json!({
            "index": 3,
            "type": "DomainSuffix",
            "payload": "example.com",
            "proxy": "PROXY",
            "size": -1,
            "extra": {
                "disabled": true,
                "hitCount": 12,
                "hitAt": "2024-01-01T00:00:00Z",
                "missCount": 4,
                "missAt": "2024-01-01T00:00:00Z",
            },
        }))
        .unwrap()
    }

    #[test]
    fn a_rule_row_carries_the_index_the_api_wants() {
        let row = RuleRow::from(&rule());
        assert_eq!(row.index, 3);
        assert!(row.disabled);
        assert_eq!(row.hit_count, 12);
        assert_eq!(row.evaluations, 16, "hits and misses together");
    }

    #[test]
    fn filtering_to_disabled_rules_still_reports_the_total() {
        let enabled: RuleInfo = serde_json::from_value(serde_json::json!({
            "index": 1, "type": "MATCH", "payload": "", "proxy": "DIRECT", "size": 0,
        }))
        .unwrap();
        let rows: Vec<RuleRow> = [enabled, rule()]
            .iter()
            .map(RuleRow::from)
            .filter(|row| row.disabled)
            .collect();
        let report = RuleListReport {
            total: 2,
            disabled: 1,
            shown: rows.len(),
            filtered: true,
            with_stats: true,
            rules: rows,
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("1 of 2 rule(s)"), "{text}");
        assert!(text.contains("disabled"), "{text}");
        let value = crate::output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.rules.list.v1"));
        assert_eq!(value["rules"][0]["index"], serde_json::json!(3));
    }

    #[test]
    fn a_listing_with_nothing_to_show_says_why() {
        let filtered = RuleListReport {
            total: 5,
            disabled: 0,
            shown: 0,
            filtered: true,
            with_stats: false,
            rules: Vec::new(),
        };
        assert!(
            filtered
                .render(Output::new(false, 0, false))
                .contains("no disabled rules")
        );
        let empty = RuleListReport {
            total: 0,
            disabled: 0,
            shown: 0,
            filtered: false,
            with_stats: false,
            rules: Vec::new(),
        };
        assert!(
            empty
                .render(Output::new(false, 0, false))
                .contains("no rules")
        );
    }

    #[test]
    fn a_toggle_reads_as_the_state_it_left_the_rule_in() {
        let report = RuleToggleReport {
            index: 3,
            kind: "DomainSuffix".into(),
            payload: "example.com".into(),
            proxy: "PROXY".into(),
            disabled: true,
        };
        assert!(
            report
                .render(Output::new(false, 0, false))
                .ends_with("disabled")
        );
    }

    #[test]
    fn the_provider_listing_is_sorted_by_name_and_keeps_the_counts() {
        let provider: RuleProviderInfo = serde_json::from_value(serde_json::json!({
            "name": "reject",
            "behavior": "domain",
            "format": "mrs",
            "ruleCount": 42,
            "vehicleType": "HTTP",
            "updatedAt": "2024-01-01T00:00:00Z",
        }))
        .unwrap();
        let report = RuleProvidersReport {
            providers: vec![RuleProviderRow::from((&"reject".to_owned(), &provider))],
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("42"), "{text}");
        assert!(text.contains("mrs"), "{text}");
    }

    #[test]
    fn a_failed_provider_update_is_listed_rather_than_hidden() {
        let report = ProviderUpdateReport {
            updated: vec!["ok".into()],
            failed: vec![ProviderFailure {
                name: "bad".into(),
                error: "connection refused".into(),
            }],
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("refreshed 1 provider(s)"), "{text}");
        assert!(text.contains("bad: connection refused"), "{text}");
    }
}
