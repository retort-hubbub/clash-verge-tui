//! `status` — one screen of facts about the installation.
//!
//! This is the command a user runs when something is wrong, so it answers even
//! when half the installation is missing: a failure to generate the
//! configuration is *part of the report*, not an error that aborts it. The
//! exit code is zero whenever the report could be gathered; `doctor` is the
//! command that fails.

use anyhow::Result;
use cvt_core::profile::store::document_path;
use serde::Serialize;
use std::fmt::Write as _;

use crate::commands::{CoreInfo, GeneratedInfo};
use crate::context::Ctx;
use crate::output::{self, Fields, Output, Report};

/// Report the installation's state.
///
/// # Errors
/// Only when the report cannot be written.
pub async fn run(ctx: &Ctx) -> Result<()> {
    let report = gather(ctx).await;
    ctx.out().emit(&report)
}

/// Everything `status` prints.
#[derive(Debug, Serialize)]
pub struct StatusReport {
    /// Application home.
    pub home: String,
    /// Core process state, binary and version.
    pub core: CoreInfo,
    /// The profile supplying the base document.
    pub profile: Option<ProfileInfo>,
    /// The generated configuration handed to the core.
    pub runtime_config: PathInfo,
    /// Summary of what generating would produce.
    pub generated: Option<GeneratedInfo>,
    /// Why generating failed, when it did.
    pub generation_error: Option<String>,
    /// The controller, as far as it could be determined.
    pub controller: Option<ControllerInfo>,
    /// Anything else worth knowing.
    pub notes: Vec<String>,
}

/// A path and whether it exists.
#[derive(Debug, Serialize)]
pub struct PathInfo {
    /// The path itself.
    pub path: String,
    /// Whether something is there.
    pub exists: bool,
}

/// The profile that supplies the base document.
#[derive(Debug, Serialize)]
pub struct ProfileInfo {
    /// Profile uid.
    pub uid: String,
    /// Display name.
    pub name: String,
    /// Profile type.
    pub kind: &'static str,
    /// Last successful refresh, as a unix timestamp.
    pub updated: Option<i64>,
    /// Path of the profile's document.
    pub document: String,
    /// Whether the document is readable.
    pub document_readable: bool,
}

/// What is known about the controller.
#[derive(Debug, Serialize)]
pub struct ControllerInfo {
    /// `host:port`, or the socket path.
    pub endpoint: String,
    /// Whether the controller is only reachable from this machine.
    pub loopback: bool,
    /// Whether a secret is required.
    pub authenticated: bool,
    /// Whether the controller answered.
    pub reachable: bool,
    /// The running core's version, when it answered.
    pub version: Option<String>,
    /// Why it did not answer, when it did not.
    pub error: Option<String>,
}

impl Report for StatusReport {
    fn schema(&self) -> &'static str {
        "cvt.status.v1"
    }

    fn render(&self, out: Output) -> String {
        let mut fields = Fields::new();
        fields.push("home", &self.home);
        fields.append(&self.core.fields());
        match &self.profile {
            Some(profile) => {
                fields.push(
                    "profile",
                    format!("{} ({}, {})", profile.name, profile.uid, profile.kind),
                );
                fields.push("updated", output::timestamp(profile.updated));
                if !profile.document_readable {
                    fields.push("document", format!("unreadable: {}", profile.document));
                }
            }
            None => fields.push("profile", "none selected"),
        }
        fields.push(
            "runtime config",
            format!(
                "{} ({})",
                self.runtime_config.path,
                if self.runtime_config.exists {
                    "present"
                } else {
                    "not written yet"
                }
            ),
        );
        match &self.generated {
            Some(generated) => fields.append(&generated.fields()),
            None => fields.push("configuration", "could not be generated"),
        }
        match &self.controller {
            Some(controller) => {
                let state = if controller.reachable {
                    "reachable"
                } else {
                    "unreachable"
                };
                fields.push(
                    "controller",
                    format!(
                        "{} ({state}, {}, {})",
                        controller.endpoint,
                        if controller.loopback {
                            "loopback"
                        } else {
                            "exposed"
                        },
                        if controller.authenticated {
                            "secret required"
                        } else {
                            "no secret"
                        }
                    ),
                );
                fields.push_opt("core version", controller.version.clone());
                fields.push_opt(
                    "controller error",
                    if controller.reachable {
                        None
                    } else {
                        controller.error.clone()
                    },
                );
            }
            None => fields.push("controller", "not configured"),
        }

        let mut text = fields.render();
        if let Some(error) = &self.generation_error {
            let _ = write!(text, "\n\ngeneration failed: {error}");
        }
        if let Some(generated) = &self.generated
            && !generated.diagnostics.is_empty()
        {
            text.push_str("\n\n");
            text.push_str(&generated.render_findings(out));
        }
        if !self.notes.is_empty() {
            text.push_str("\n\n");
            for note in &self.notes {
                let _ = writeln!(text, "note: {note}");
            }
            text.truncate(text.trim_end().len());
        }
        text
    }
}

async fn gather(ctx: &Ctx) -> StatusReport {
    let mut notes: Vec<String> = Vec::new();

    let store = match ctx.store() {
        Ok(store) => Some(store),
        Err(error) => {
            notes.push(format!("the profile index could not be read: {error}"));
            None
        }
    };

    let profile = store.as_ref().and_then(|store| {
        store.current().map(|item| ProfileInfo {
            uid: item.uid.clone(),
            name: item.label().to_owned(),
            kind: item.kind.as_str(),
            updated: item.updated,
            document: document_path(ctx.paths(), item).display().to_string(),
            document_readable: store.read_document(item).is_ok(),
        })
    });
    if profile.is_none() && store.is_some() {
        notes
            .push("no profile is selected; run `clash-verge-tui profiles switch <uid>`".to_owned());
    }
    if let Some(profile) = &profile
        && !profile.document_readable
    {
        notes.push(format!(
            "the document for `{}` is missing; refresh it with `clash-verge-tui profiles update {}`",
            profile.name, profile.uid
        ));
    }

    let mut generated = None;
    let mut generation_error = None;
    match ctx.service().generate() {
        Ok(outcome) => {
            if let Some(skipped) = outcome.skipped().first() {
                notes.push(format!("`{}` was skipped: {}", skipped.name, skipped.note));
            }
            generated = Some(GeneratedInfo::from(&outcome));
        }
        Err(error) => generation_error = Some(error.to_string()),
    }

    let core = CoreInfo::gather(ctx);
    if core.binary.is_none() {
        notes.push(
            "no core binary was found; put one in the core directory or set CVT_CORE".to_owned(),
        );
    }

    let runtime = ctx.paths().runtime_config();
    let controller = controller_info(ctx).await;

    StatusReport {
        home: ctx.paths().home().display().to_string(),
        core,
        profile,
        runtime_config: PathInfo {
            path: runtime.display().to_string(),
            exists: runtime.is_file(),
        },
        generated,
        generation_error,
        controller,
        notes,
    }
}

/// Ask the controller what it is, without letting a refusal abort the report.
async fn controller_info(ctx: &Ctx) -> Option<ControllerInfo> {
    let endpoint = ctx.service().endpoint().ok().flatten()?;
    let mut info = ControllerInfo {
        endpoint: endpoint.describe(),
        loopback: endpoint.is_loopback(),
        authenticated: endpoint.is_authenticated(),
        reachable: false,
        version: None,
        error: None,
    };
    match ctx.client() {
        Ok(client) => match client.version().await {
            Ok(version) => {
                info.reachable = true;
                info.version = Some(version.trimmed().to_owned());
            }
            Err(error) => info.error = Some(error.short()),
        },
        Err(error) => info.error = Some(error.to_string()),
    }
    Some(info)
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

    #[tokio::test]
    async fn an_empty_home_still_produces_a_report() {
        let dir = TempDir::new().unwrap();
        let report = gather(&ctx(&dir)).await;
        assert!(report.profile.is_none());
        assert!(report.generated.is_none());
        assert!(report.generation_error.is_some(), "nothing to generate");
        assert!(report.controller.is_none(), "no endpoint is configured");
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("no profile is selected")),
            "{:?}",
            report.notes
        );
        assert_eq!(report.core.state, "stopped", "a home without a pid file");
        assert!(report.core.binary.is_none());
    }

    #[tokio::test]
    async fn the_json_shape_is_stable() {
        let dir = TempDir::new().unwrap();
        let report = gather(&ctx(&dir)).await;
        let value = output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.status.v1"));
        assert_eq!(value["core"]["state"], serde_json::json!("stopped"));
        assert_eq!(value["profile"], serde_json::Value::Null);
        assert!(value["runtime_config"]["path"].is_string());
        assert_eq!(value["runtime_config"]["exists"], serde_json::json!(false));
        assert!(value["notes"].is_array());
        assert!(value["generation_error"].is_string());
    }

    #[tokio::test]
    async fn the_rendering_never_claims_a_profile_that_is_absent() {
        let dir = TempDir::new().unwrap();
        let text = gather(&ctx(&dir))
            .await
            .render(Output::new(false, 0, false));
        assert!(text.contains("profile"), "{text}");
        assert!(text.contains("none selected"), "{text}");
        assert!(text.contains("not written yet"), "{text}");
        assert!(text.contains("not configured"), "{text}");
        assert!(text.contains("generation failed"), "{text}");
    }
}
