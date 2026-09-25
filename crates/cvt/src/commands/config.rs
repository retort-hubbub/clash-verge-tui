//! `config` — the generated runtime configuration.
//!
//! Everything here is about one file: what *would* be written, what *is*
//! written, and how to get back to a version that worked. `generate` without
//! `--apply` is deliberately read-only — it answers "what would change?" — and
//! only `--apply` writes.

use anyhow::{Context as _, Result};
use cvt_core::enhance::pipeline::Pipeline;
use cvt_core::error::Error;
use cvt_core::service::{ReloadMode, ReloadOutcome};
use serde::Serialize;

use crate::cli::{ConfigCommand, GenerateArgs};
use crate::commands::{DiffInfo, GeneratedInfo};
use crate::context::Ctx;
use crate::exit::{Exit, ExitCode};
use crate::output::{self, Fields, Output, Report, Table};

/// Run one `config` subcommand.
///
/// # Errors
/// Whatever the operation failed with.
pub async fn run(ctx: &Ctx, command: &ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Generate(args) => generate(ctx, args).await,
        ConfigCommand::Show => show(ctx),
        ConfigCommand::Validate => validate(ctx),
        ConfigCommand::Diff => diff(ctx),
        ConfigCommand::Rollback => rollback(ctx),
        ConfigCommand::Snapshots => snapshots(ctx),
        ConfigCommand::Edit => edit(ctx),
        ConfigCommand::Path => path(ctx),
    }
}

/// What happened when a configuration was generated, and possibly deployed.
#[derive(Debug, Serialize)]
pub struct GenerateReport {
    /// Whether the document was handed to the core.
    pub applied: bool,
    /// Whether it was written to disk.
    pub written: bool,
    /// How the change reached the core, when it was applied.
    pub reload: Option<ReloadInfo>,
    /// The generated document.
    pub generated: GeneratedInfo,
    /// The rendered YAML, exactly as it would be written.
    pub yaml: String,
}

/// How a change reached the core.
#[derive(Debug, Serialize)]
pub struct ReloadInfo {
    /// `hot-reloaded`, `restarted` or `rolled-back`.
    pub kind: &'static str,
    /// Whether the core is running the requested configuration.
    pub succeeded: bool,
    /// The library's one-line description.
    pub summary: String,
    /// The snapshot that was restored, when a rollback happened.
    pub snapshot: Option<String>,
}

impl From<&ReloadOutcome> for ReloadInfo {
    fn from(outcome: &ReloadOutcome) -> Self {
        let (kind, snapshot) = match outcome {
            ReloadOutcome::HotReloaded => ("hot-reloaded", None),
            ReloadOutcome::Restarted { .. } => ("restarted", None),
            ReloadOutcome::RolledBack { snapshot, .. } => {
                ("rolled-back", Some(snapshot.display().to_string()))
            }
        };
        Self {
            kind,
            succeeded: outcome.succeeded(),
            summary: outcome.summary(),
            snapshot,
        }
    }
}

impl Report for GenerateReport {
    fn schema(&self) -> &'static str {
        "cvt.config.generate.v1"
    }

    fn render(&self, out: Output) -> String {
        let mut fields = Fields::new();
        fields.push("written", output::yes_no(self.written));
        fields.push_opt("reload", self.reload.as_ref().map(|r| r.summary.clone()));
        fields.append(&self.generated.fields());
        let mut text = fields.render();
        text.push_str("\n\n");
        text.push_str(&self.generated.render_findings(out));
        text.push_str(
            "\n\nrun `clash-verge-tui config show` for the document, or add --json to get it inline",
        );
        text
    }
}

async fn generate(ctx: &Ctx, args: &GenerateArgs) -> Result<()> {
    if args.apply {
        // A reload can only fall back to restarting the process, and a restart
        // needs a binary. Checking that first turns the most common first-run
        // failure into the documented exit code and a message that says what
        // to install, instead of a report about rollback bookkeeping.
        if !ctx.service().core_status().is_running() && ctx.service().core_binary().is_none() {
            return Err(Error::CoreUnavailable {
                reason: format!(
                    "no mihomo binary found; put one at {} or set {}",
                    ctx.paths().core_dir().join("mihomo").display(),
                    cvt_core::mihomo::supervisor::CORE_ENV
                ),
            }
            .into());
        }
        let report = match ctx
            .service()
            .apply(args.force, ReloadMode::from(args.mode))
            .await
        {
            Ok(report) => report,
            // `Service::reload` reports a failed rollback *in place of* the
            // failure that caused it, so on a first apply — where there is no
            // snapshot to restore — the useful reason is gone by the time it
            // reaches here. Say what is still knowable and point at the command
            // that can recover the reason.
            Err(Error::InvalidValue {
                field: "rollback",
                reason,
            }) => {
                return Err(Exit::failure(format!(
                    "the core did not accept the new configuration ({reason}); \
                     the document was written but is not deployed — run `clash-verge-tui doctor` \
                     to see why the core refused it"
                ))
                .into());
            }
            Err(error) => return Err(error).context("could not apply the configuration"),
        };
        let reload = report.reload.as_ref().map(ReloadInfo::from);
        let generated = GeneratedInfo::from(&report.outcome);
        let failed = reload.as_ref().is_some_and(|r| !r.succeeded);
        ctx.out().emit(&GenerateReport {
            applied: true,
            written: report.written,
            reload,
            generated,
            yaml: report.outcome.yaml,
        })?;
        if failed {
            return Err(Exit::failure(
                "the core rejected the new configuration and the previous one was restored",
            )
            .into());
        }
        return Ok(());
    }

    let outcome = ctx
        .service()
        .generate()
        .context("could not generate the configuration")?;
    let applicable = outcome.is_applicable();
    let errors = outcome.report.errors();
    ctx.out().emit(&GenerateReport {
        applied: false,
        written: false,
        reload: None,
        generated: GeneratedInfo::from(&outcome),
        yaml: outcome.yaml,
    })?;
    if !applicable {
        return Err(Exit::new(
            ExitCode::Validation,
            format!("the generated configuration has {errors} error(s); nothing was written"),
        )
        .into());
    }
    Ok(())
}

/// The deployed document.
#[derive(Debug, Serialize)]
pub struct ShowReport {
    /// Path of the document.
    pub path: String,
    /// Always true; a missing document is an error.
    pub exists: bool,
    /// The document itself.
    pub yaml: String,
}

impl Report for ShowReport {
    fn schema(&self) -> &'static str {
        "cvt.config.show.v1"
    }

    fn render(&self, _out: Output) -> String {
        if self.yaml.ends_with('\n') {
            self.yaml.clone()
        } else {
            format!("{}\n", self.yaml)
        }
    }
}

fn show(ctx: &Ctx) -> Result<()> {
    let path = ctx.paths().runtime_config();
    if !path.is_file() {
        return Err(Error::invalid(
            "runtime config",
            format!(
                "{} does not exist yet; run `clash-verge-tui config generate --apply`",
                path.display()
            ),
        )
        .into());
    }
    let yaml = ctx.paths().read(&path)?;
    ctx.out().emit(&ShowReport {
        path: path.display().to_string(),
        exists: true,
        yaml,
    })
}

/// The result of `config validate`.
#[derive(Debug, Serialize)]
pub struct ValidateReport {
    /// Whether the document may be handed to the core.
    pub ok: bool,
    /// The generated document's summary.
    pub generated: GeneratedInfo,
}

impl Report for ValidateReport {
    fn schema(&self) -> &'static str {
        "cvt.config.validate.v1"
    }

    fn render(&self, out: Output) -> String {
        let mut text = self.generated.fields().render();
        text.push_str("\n\n");
        text.push_str(&self.generated.render_findings(out));
        text
    }
}

fn validate(ctx: &Ctx) -> Result<()> {
    let outcome = ctx
        .service()
        .generate()
        .context("could not generate the configuration")?;
    let ok = outcome.is_applicable();
    let errors = outcome.report.errors();
    let generated = GeneratedInfo::from(&outcome);
    ctx.out().emit(&ValidateReport { ok, generated })?;
    if !ok {
        return Err(Exit::new(
            ExitCode::Validation,
            format!("the generated configuration has {errors} error(s)"),
        )
        .into());
    }
    Ok(())
}

/// The difference between what would be generated and what is deployed.
///
/// The payload is [`DiffInfo`]; the wrapper exists only so that the JSON
/// `schema` names this command.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct ConfigDiffReport(pub DiffInfo);

impl Report for ConfigDiffReport {
    fn schema(&self) -> &'static str {
        "cvt.config.diff.v1"
    }

    fn render(&self, _out: Output) -> String {
        self.0.text.clone()
    }
}

fn diff(ctx: &Ctx) -> Result<()> {
    let outcome = ctx
        .service()
        .generate()
        .context("could not generate the configuration")?;
    ctx.out()
        .emit(&ConfigDiffReport(DiffInfo::from(&outcome.diff)))
}

/// What `config rollback` restored.
#[derive(Debug, Serialize)]
pub struct RollbackReport {
    /// The snapshot that was put back.
    pub restored: String,
    /// Where it was written.
    pub deployed: String,
}

impl Report for RollbackReport {
    fn schema(&self) -> &'static str {
        "cvt.config.rollback.v1"
    }

    fn render(&self, _out: Output) -> String {
        format!("restored {} to {}", self.restored, self.deployed)
    }
}

fn rollback(ctx: &Ctx) -> Result<()> {
    let restored = ctx.service().pipeline().rollback()?;
    ctx.out().emit(&RollbackReport {
        restored: restored.display().to_string(),
        deployed: ctx.paths().runtime_config().display().to_string(),
    })
}

/// One snapshot.
#[derive(Debug, Serialize)]
pub struct SnapshotRow {
    /// Path of the snapshot.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Seconds since it was written.
    pub age_seconds: Option<i64>,
}

/// The snapshot list.
#[derive(Debug, Serialize)]
pub struct SnapshotsReport {
    /// Where snapshots are kept.
    pub directory: String,
    /// How many are kept before the oldest is dropped.
    pub limit: usize,
    /// Snapshots, newest first.
    pub snapshots: Vec<SnapshotRow>,
}

impl Report for SnapshotsReport {
    fn schema(&self) -> &'static str {
        "cvt.config.snapshots.v1"
    }

    fn render(&self, _out: Output) -> String {
        if self.snapshots.is_empty() {
            return format!("no snapshots in {}", self.directory);
        }
        let mut table = Table::new(["path", "size", "age"]);
        for row in &self.snapshots {
            table.push([
                row.path.clone(),
                output::bytes(row.size),
                row.age_seconds.map_or_else(|| "-".to_owned(), output::age),
            ]);
        }
        format!(
            "{}\n\n{} snapshot(s), newest first; at most {} are kept",
            table.render(),
            self.snapshots.len(),
            self.limit
        )
    }
}

fn snapshots(ctx: &Ctx) -> Result<()> {
    let paths = ctx.service().pipeline().snapshots()?;
    let rows = paths
        .iter()
        .map(|path| {
            let metadata = std::fs::metadata(path).ok();
            SnapshotRow {
                path: path.display().to_string(),
                size: metadata.as_ref().map_or(0, std::fs::Metadata::len),
                age_seconds: metadata
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)),
            }
        })
        .collect();
    ctx.out().emit(&SnapshotsReport {
        directory: ctx.paths().snapshots_dir().display().to_string(),
        limit: Pipeline::SNAPSHOT_LIMIT,
        snapshots: rows,
    })
}

/// What `config edit` opened.
#[derive(Debug, Serialize)]
pub struct EditReport {
    /// The command that was run.
    pub editor: String,
    /// The file it was pointed at.
    pub path: String,
    /// The editor's exit status.
    pub status: i32,
    /// Whether it exited successfully.
    pub success: bool,
}

impl Report for EditReport {
    fn schema(&self) -> &'static str {
        "cvt.config.edit.v1"
    }

    fn render(&self, _out: Output) -> String {
        format!("{} {} (exit {})", self.editor, self.path, self.status)
    }
}

fn edit(ctx: &Ctx) -> Result<()> {
    let path = ctx.paths().runtime_config();
    if !path.is_file() {
        return Err(Error::invalid(
            "runtime config",
            format!(
                "{} does not exist yet; generate it first with `clash-verge-tui config generate --apply`",
                path.display()
            ),
        )
        .into());
    }
    let command = editor_from(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok());
    let Some((program, args)) = command.split_first() else {
        return Err(Error::invalid("editor", "no editor command").into());
    };
    let status = std::process::Command::new(program)
        .args(args)
        .arg(&path)
        .status()
        .with_context(|| format!("could not start `{program}`"))?;
    let report = EditReport {
        editor: command.join(" "),
        path: path.display().to_string(),
        status: status.code().unwrap_or(-1),
        success: status.success(),
    };
    let success = report.success;
    let code = report.status;
    ctx.out().emit(&report)?;
    if !success {
        return Err(Exit::failure(format!("the editor exited with status {code}")).into());
    }
    ctx.out().note(
        "the runtime configuration is regenerated on every apply; put lasting changes in an \
         `override` profile instead",
    );
    Ok(())
}

/// The editor to run: `$VISUAL`, then `$EDITOR`, then `vi`.
///
/// A value with spaces is split, so `EDITOR="code --wait"` works the way
/// everyone expects it to.
#[must_use]
pub fn editor_from(visual: Option<String>, editor: Option<String>) -> Vec<String> {
    for value in [visual, editor].into_iter().flatten() {
        let parts: Vec<String> = value.split_whitespace().map(str::to_owned).collect();
        if !parts.is_empty() {
            return parts;
        }
    }
    vec!["vi".to_owned()]
}

/// The paths the application uses.
#[derive(Debug, Serialize)]
pub struct PathsReport {
    /// Application home.
    pub home: String,
    /// Settings file.
    pub settings: String,
    /// Profile index.
    pub profiles_index: String,
    /// Downloaded and local profile documents.
    pub profiles_dir: String,
    /// Declarative override profiles.
    pub overrides_dir: String,
    /// Generated configuration.
    pub runtime_config: String,
    /// Previous generated configuration.
    pub runtime_config_previous: String,
    /// Snapshots.
    pub snapshots_dir: String,
    /// The core binary and its data.
    pub core_dir: String,
    /// The core's captured output.
    pub core_log: String,
    /// The application's own log.
    pub app_log: String,
}

impl Report for PathsReport {
    fn schema(&self) -> &'static str {
        "cvt.config.path.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut fields = Fields::new();
        for (label, value) in [
            ("home", self.home.as_str()),
            ("settings", self.settings.as_str()),
            ("profile index", self.profiles_index.as_str()),
            ("profiles", self.profiles_dir.as_str()),
            ("overrides", self.overrides_dir.as_str()),
            ("runtime config", self.runtime_config.as_str()),
            ("previous config", self.runtime_config_previous.as_str()),
            ("snapshots", self.snapshots_dir.as_str()),
            ("core", self.core_dir.as_str()),
            ("core log", self.core_log.as_str()),
            ("app log", self.app_log.as_str()),
        ] {
            fields.push(label, value);
        }
        fields.render()
    }
}

fn path(ctx: &Ctx) -> Result<()> {
    let paths = ctx.paths();
    ctx.out().emit(&PathsReport {
        home: paths.home().display().to_string(),
        settings: paths.settings_file().display().to_string(),
        profiles_index: paths.profiles_index().display().to_string(),
        profiles_dir: paths.profiles_dir().display().to_string(),
        overrides_dir: paths.overrides_dir().display().to_string(),
        runtime_config: paths.runtime_config().display().to_string(),
        runtime_config_previous: paths.runtime_config_previous().display().to_string(),
        snapshots_dir: paths.snapshots_dir().display().to_string(),
        core_dir: paths.core_dir().display().to_string(),
        core_log: paths.core_log().display().to_string(),
        app_log: paths.app_log().display().to_string(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cvt_core::AppPaths;
    use cvt_core::profile::item::PrfItem;
    use tempfile::TempDir;

    const BASE: &str = "mixed-port: 7890\nexternal-controller: 127.0.0.1:9090\nmode: rule\nproxies:\n  - { name: a, type: vless, server: 1.2.3.4, port: 443, uuid: u }\nproxy-groups:\n  - { name: G, type: select, proxies: [a] }\nrules:\n  - MATCH,G\n";

    fn ctx(dir: &TempDir) -> Ctx {
        Ctx::open(AppPaths::new(dir.path()), Output::new(false, 0, false)).unwrap()
    }

    fn seed(ctx: &Ctx) {
        ctx.edit_store(|store| {
            let uid = store.add(PrfItem::local("L1", "base"));
            let item = store.get(&uid).unwrap().clone();
            store.write_document(&item, BASE)?;
            store.set_current(&uid)
        })
        .unwrap();
    }

    #[test]
    fn the_editor_falls_back_in_the_documented_order() {
        assert_eq!(
            editor_from(Some("nano".into()), Some("vi".into())),
            vec!["nano".to_owned()]
        );
        assert_eq!(editor_from(None, Some("vi".into())), vec!["vi".to_owned()]);
        assert_eq!(
            editor_from(Some("  ".into()), Some("code --wait".into())),
            vec!["code".to_owned(), "--wait".to_owned()],
            "an empty variable must not win"
        );
        assert_eq!(editor_from(None, None), vec!["vi".to_owned()]);
    }

    #[test]
    fn generating_without_applying_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx(&dir);
        seed(&ctx);
        let before = ctx.paths().runtime_config();
        assert!(!before.exists());
        // The command path is exercised through the library call it makes.
        let outcome = ctx.service().generate().unwrap();
        assert!(outcome.is_applicable());
        assert!(
            !ctx.paths().runtime_config().exists(),
            "generate must not write without --apply"
        );
    }

    #[test]
    fn a_missing_runtime_config_is_an_error_with_a_hint() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx(&dir);
        let error = show(&ctx).unwrap_err();
        assert!(error.to_string().contains("generate --apply"), "{error}");
    }

    #[test]
    fn the_json_shapes_are_named_per_command() {
        let generated = GeneratedInfo::from(&{
            let dir = TempDir::new().unwrap();
            let ctx = ctx(&dir);
            seed(&ctx);
            ctx.service().generate().unwrap()
        });
        let report = GenerateReport {
            applied: false,
            written: false,
            reload: None,
            generated: generated.clone(),
            yaml: "mode: rule\n".into(),
        };
        let value = output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.config.generate.v1"));
        assert_eq!(value["applied"], serde_json::json!(false));
        assert_eq!(value["yaml"], serde_json::json!("mode: rule\n"));
        assert!(value["generated"]["stats"]["rules"].is_number());

        let validate = ValidateReport {
            ok: true,
            generated,
        };
        assert_eq!(
            output::to_value(&validate).unwrap()["schema"],
            serde_json::json!("cvt.config.validate.v1")
        );

        let diff = ConfigDiffReport(DiffInfo::from(&cvt_core::enhance::diff::Diff::default()));
        let value = output::to_value(&diff).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.config.diff.v1"));
        assert_eq!(value["empty"], serde_json::json!(true));
        assert_eq!(diff.render(Output::new(false, 0, false)), "no changes\n");
    }

    #[test]
    fn the_paths_report_covers_every_directory_the_application_uses() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx(&dir);
        let paths = ctx.paths();
        let report = PathsReport {
            home: paths.home().display().to_string(),
            settings: paths.settings_file().display().to_string(),
            profiles_index: paths.profiles_index().display().to_string(),
            profiles_dir: paths.profiles_dir().display().to_string(),
            overrides_dir: paths.overrides_dir().display().to_string(),
            runtime_config: paths.runtime_config().display().to_string(),
            runtime_config_previous: paths.runtime_config_previous().display().to_string(),
            snapshots_dir: paths.snapshots_dir().display().to_string(),
            core_dir: paths.core_dir().display().to_string(),
            core_log: paths.core_log().display().to_string(),
            app_log: paths.app_log().display().to_string(),
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("runtime config"), "{text}");
        assert!(text.contains("core log"), "{text}");
        let value = output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.config.path.v1"));
        assert!(
            value["runtime_config"]
                .as_str()
                .unwrap()
                .ends_with("config.yaml")
        );
    }

    #[test]
    fn snapshots_are_reported_newest_first_with_their_age() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx(&dir);
        seed(&ctx);
        let pipeline = ctx.service().pipeline();
        let outcome = ctx.service().generate().unwrap();
        pipeline.commit(&outcome, false).unwrap();
        // A second commit is what snapshots the first.
        let second = ctx.service().generate().unwrap();
        pipeline.commit(&second, false).unwrap();
        assert!(
            !pipeline.snapshots().unwrap().is_empty(),
            "committing twice must leave a snapshot behind"
        );

        let report = SnapshotsReport {
            directory: ctx.paths().snapshots_dir().display().to_string(),
            limit: Pipeline::SNAPSHOT_LIMIT,
            snapshots: vec![SnapshotRow {
                path: "/home/u/snapshots/config.1.yaml".into(),
                size: 2048,
                age_seconds: Some(90),
            }],
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("2.0 KiB"), "{text}");
        assert!(text.contains("1m ago"), "{text}");
        assert!(text.contains("newest first"), "{text}");
        let value = output::to_value(&report).unwrap();
        assert_eq!(
            value["schema"],
            serde_json::json!("cvt.config.snapshots.v1")
        );
        assert_eq!(value["snapshots"][0]["size"], serde_json::json!(2048));
    }

    #[test]
    fn an_empty_snapshot_list_says_where_it_looked() {
        let report = SnapshotsReport {
            directory: "/nowhere".into(),
            limit: 20,
            snapshots: Vec::new(),
        };
        assert_eq!(
            report.render(Output::new(false, 0, false)),
            "no snapshots in /nowhere"
        );
    }
}
