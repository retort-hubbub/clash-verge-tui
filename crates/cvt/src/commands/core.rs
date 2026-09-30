//! `core` — the mihomo process.
//!
//! Starting and stopping are the supervisor's job; what this module adds is the
//! reporting around them, and the rule that anything asking the *running* core
//! to do something (`upgrade`, `geo`, `gc`) goes through the controller, not
//! through the process.

use anyhow::Result;
use cvt_core::error::Error;
use serde::Serialize;

use crate::cli::{CoreCommand, UpgradeArgs};
use crate::commands::CoreInfo;
use crate::context::Ctx;
use crate::output::{self, Fields, Output, Report};

/// Run one `core` subcommand.
///
/// # Errors
/// Whatever the operation failed with.
pub async fn run(ctx: &Ctx, command: &CoreCommand) -> Result<()> {
    match command {
        CoreCommand::Status => status(ctx),
        CoreCommand::Start => start(ctx),
        CoreCommand::Stop => stop(ctx),
        CoreCommand::Restart => restart(ctx),
        CoreCommand::Version => version(ctx).await,
        CoreCommand::Upgrade(args) => upgrade(ctx, args).await,
        CoreCommand::Geo => geo(ctx).await,
        CoreCommand::Gc => gc(ctx).await,
    }
}

fn status(ctx: &Ctx) -> Result<()> {
    ctx.out().emit(&CoreInfo::gather(ctx))
}

/// What a process action did.
#[derive(Debug, Serialize)]
pub struct CoreActionReport {
    /// `started`, `stopped`, `restarted` or `already-stopped`.
    pub action: &'static str,
    /// Process id, when there is one afterwards.
    pub pid: Option<u32>,
    /// Anything else worth saying.
    pub detail: String,
}

impl Report for CoreActionReport {
    fn schema(&self) -> &'static str {
        "cvt.core.action.v1"
    }

    fn render(&self, _out: Output) -> String {
        match self.pid {
            Some(pid) => format!("{} (pid {pid})", self.action),
            None => self.action.to_owned(),
        }
    }
}

fn start(ctx: &Ctx) -> Result<()> {
    authorize_listeners(ctx)?;
    let pid = ctx.service().start_core()?;
    ctx.out().emit(&CoreActionReport {
        action: "started",
        pid: Some(pid),
        detail: "the core is running the deployed configuration".to_owned(),
    })
}

fn stop(ctx: &Ctx) -> Result<()> {
    let stopped = ctx.service().stop_core()?;
    ctx.out().emit(&CoreActionReport {
        action: if stopped {
            "stopped"
        } else {
            "already-stopped"
        },
        pid: None,
        detail: if stopped {
            "the process was asked to stop, then killed if it did not".to_owned()
        } else {
            "nothing was running".to_owned()
        },
    })
}

fn restart(ctx: &Ctx) -> Result<()> {
    authorize_listeners(ctx)?;
    let pid = ctx.service().restart_core()?;
    ctx.out().emit(&CoreActionReport {
        action: "restarted",
        pid: Some(pid),
        detail: "the new process was given the deployed configuration".to_owned(),
    })
}

/// Where the binary's version and the running core's version come from.
#[derive(Debug, Serialize)]
pub struct VersionReport {
    /// Path of the binary.
    pub binary: Option<String>,
    /// What `mihomo -v` printed.
    pub binary_version: Option<String>,
    /// Why the binary could not be asked.
    pub binary_error: Option<String>,
    /// The controller endpoint.
    pub endpoint: Option<String>,
    /// What the running core reports.
    pub api_version: Option<String>,
    /// Why the running core could not be asked.
    pub api_error: Option<String>,
}

impl Report for VersionReport {
    fn schema(&self) -> &'static str {
        "cvt.core.version.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut fields = Fields::new();
        fields.push_opt("binary", self.binary.clone());
        fields.push_opt(
            "binary version",
            self.binary_version
                .clone()
                .or_else(|| self.binary_error.clone()),
        );
        if self.endpoint.is_some() {
            fields.push_opt("controller", self.endpoint.clone());
            fields.push_opt(
                "running core",
                self.api_version.clone().or_else(|| self.api_error.clone()),
            );
        }
        fields.render()
    }
}

async fn version(ctx: &Ctx) -> Result<()> {
    let mut report = VersionReport {
        binary: None,
        binary_version: None,
        binary_error: None,
        endpoint: None,
        api_version: None,
        api_error: None,
    };
    match ctx.service().core_binary() {
        Some(binary) => {
            report.binary = Some(binary.display().to_string());
            match ctx.service().supervisor().version(&binary) {
                Ok(text) => report.binary_version = Some(text),
                Err(error) => report.binary_error = Some(error.to_string()),
            }
        }
        None => {
            report.binary_error = Some("no binary was found".to_owned());
        }
    }
    if let Ok(Some(endpoint)) = ctx.service().endpoint() {
        report.endpoint = Some(endpoint.describe());
        match ctx.client() {
            Ok(client) => match client.version().await {
                Ok(version) => report.api_version = Some(version.trimmed().to_owned()),
                Err(error) => report.api_error = Some(error.short()),
            },
            Err(error) => report.api_error = Some(error.to_string()),
        }
    }
    ctx.out().emit(&report)
}

/// The result of a request the core carries out on itself.
#[derive(Debug, Serialize)]
pub struct CoreRequestReport {
    /// `upgrade`, `geo` or `gc`.
    pub request: &'static str,
    /// The endpoint it was sent to.
    pub endpoint: String,
    /// What was asked for.
    pub detail: String,
}

impl Report for CoreRequestReport {
    fn schema(&self) -> &'static str {
        "cvt.core.request.v1"
    }

    fn render(&self, _out: Output) -> String {
        format!(
            "{request} accepted by {endpoint}",
            request = self.request,
            endpoint = self.endpoint
        )
    }
}

async fn upgrade(ctx: &Ctx, args: &UpgradeArgs) -> Result<()> {
    let client = ctx.client()?;
    let endpoint = ctx
        .service()
        .endpoint()?
        .ok_or_else(|| Error::invalid("external-controller", "no endpoint is configured"))?;
    client
        .upgrade_core(Some(args.channel.as_str()), args.force)
        .await?;
    ctx.out().emit(&CoreRequestReport {
        request: "upgrade",
        endpoint: endpoint.describe(),
        detail: format!(
            "channel {}, force {}",
            args.channel.as_str(),
            output::yes_no(args.force)
        ),
    })
}

async fn geo(ctx: &Ctx) -> Result<()> {
    let client = ctx.client()?;
    let endpoint = ctx
        .service()
        .endpoint()?
        .ok_or_else(|| Error::invalid("external-controller", "no endpoint is configured"))?;
    client.upgrade_geo().await?;
    ctx.out().emit(&CoreRequestReport {
        request: "geo",
        endpoint: endpoint.describe(),
        detail: "the geo databases were refreshed".to_owned(),
    })
}

async fn gc(ctx: &Ctx) -> Result<()> {
    let client = ctx.client()?;
    let endpoint = ctx
        .service()
        .endpoint()?
        .ok_or_else(|| Error::invalid("external-controller", "no endpoint is configured"))?;
    client.force_gc().await?;
    ctx.out().emit(&CoreRequestReport {
        request: "gc",
        endpoint: endpoint.describe(),
        detail: "the core ran a garbage collection".to_owned(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cvt_core::AppPaths;
    use tempfile::TempDir;

    fn ctx(dir: &TempDir) -> Ctx {
        Ctx::open(AppPaths::new(dir.path()), Output::new(false, 0, false)).unwrap()
    }

    #[test]
    fn a_stopped_core_is_reported_as_stopped_not_as_an_error() {
        let dir = TempDir::new().unwrap();
        let report = CoreInfo::gather(&ctx(&dir));
        assert_eq!(report.state, "stopped");
        assert!(!report.running);
        assert!(report.binary.is_none());
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("stopped"), "{text}");
        assert!(text.contains("not found"), "{text}");
    }

    #[test]
    fn the_json_shapes_of_the_core_commands_are_distinct() {
        let status = CoreInfo::from_parts(
            &cvt_core::mihomo::supervisor::CoreStatus::Running { pid: 7, since: 1 },
            None,
            Some("v1.0.0".into()),
        );
        assert_eq!(
            crate::output::to_value(&status).unwrap()["schema"],
            serde_json::json!("cvt.core.status.v1")
        );
        assert_eq!(status.pid, Some(7));

        let action = CoreActionReport {
            action: "started",
            pid: Some(7),
            detail: "x".to_owned(),
        };
        assert_eq!(
            crate::output::to_value(&action).unwrap()["schema"],
            serde_json::json!("cvt.core.action.v1")
        );
        assert_eq!(
            action.render(Output::new(false, 0, false)),
            "started (pid 7)"
        );

        let stopped = CoreActionReport {
            action: "already-stopped",
            pid: None,
            detail: "x".to_owned(),
        };
        assert_eq!(
            stopped.render(Output::new(false, 0, false)),
            "already-stopped"
        );
    }

    #[test]
    fn a_version_report_keeps_both_versions_apart() {
        let report = VersionReport {
            binary: Some("/x/mihomo".into()),
            binary_version: Some("Mihomo Meta v1.19.31".into()),
            binary_error: None,
            endpoint: Some("tcp://127.0.0.1:9090".into()),
            api_version: Some("1.19.31".into()),
            api_error: None,
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("Mihomo Meta v1.19.31"), "{text}");
        assert!(text.contains("1.19.31"), "{text}");
        assert!(text.contains("tcp://127.0.0.1:9090"), "{text}");
        let value = crate::output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.core.version.v1"));
        assert_eq!(value["api_version"], serde_json::json!("1.19.31"));
    }

    #[test]
    fn a_version_report_without_a_controller_says_nothing_about_one() {
        let report = VersionReport {
            binary: None,
            binary_version: None,
            binary_error: Some("no binary was found".into()),
            endpoint: None,
            api_version: None,
            api_error: None,
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("no binary was found"), "{text}");
        assert!(!text.contains("controller"), "{text}");
    }
}

/// Authenticate on the command-line terminal before launching listeners.
fn authorize_listeners(ctx: &Ctx) -> Result<()> {
    let path = ctx.paths().runtime_config();
    let config = if path.is_file() {
        cvt_core::model::config::Config::from_yaml(&ctx.paths().read(&path)?)?
    } else {
        ctx.service().generate()?.config
    };
    let capabilities = crate::tun::required_capabilities(&config);
    if capabilities.is_empty() {
        return Ok(());
    }
    let binary = ctx
        .service()
        .core_binary()
        .ok_or_else(|| anyhow::anyhow!("install or select a Mihomo core before starting it"))?;
    if !crate::tun::has_capabilities(&binary, &capabilities)? {
        ctx.out().note(format!(
            "core listeners require {capabilities}; authenticating to grant capabilities on {}",
            binary.display()
        ));
        crate::tun::authorize_capabilities(&binary, &capabilities)?;
    }
    Ok(())
}
