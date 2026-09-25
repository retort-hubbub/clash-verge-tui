//! `doctor` — the diagnostic report.
//!
//! The order of the checks is the point of the command: nothing later can be
//! interpreted before the thing it depends on has been looked at, and the
//! report is read top-down by someone whose network is already broken. The
//! order lives in [`CHECKS`] as data, so it can be asserted rather than
//! inferred from the shape of a function.
//!
//! `doctor` is also the one command that must see the home *as it found it*.
//! Opening a service creates the directory layout, so the first checks run
//! against [`cvt_core::AppPaths`] alone and the service is opened afterwards,
//! once their result has been recorded.

use std::fmt::Write as _;
use std::path::Path;

use cvt_core::mihomo::endpoint::Endpoint;
use cvt_core::mihomo::supervisor::CORE_ENV;
use cvt_core::profile::source::is_due;
use cvt_core::profile::store::ProfileStore;
use cvt_core::{AppPaths, Service};
use serde::Serialize;
use serde_json::{Value, json};

use crate::commands::DiagnosticInfo;
use crate::context::Ctx;
use crate::exit::{Exit, ExitCode};
use crate::output::{CheckStatus, Output, Report};

/// The checks, in the order they run.
///
/// A report always holds a prefix of this list: when the settings file is
/// unusable, the checks that depend on it are not run at all, and
/// [`DoctorReport::aborted`] says why. Reporting them as "passed" would be a
/// lie, and reporting eleven copies of "not run" would bury the one line that
/// matters.
pub const CHECKS: &[&str] = &[
    "home",
    "settings",
    "core-binary",
    "core-version",
    "profile-index",
    "profile-current",
    "chain-documents",
    "config-validation",
    "core-validate",
    "controller",
    "controller-exposure",
    "subscription",
];

/// Which documented exit code a failed check implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// No more specific code applies.
    General,
    /// The generated configuration is the problem.
    Validation,
    /// The controller is the problem.
    Controller,
    /// The core binary is the problem.
    Core,
}

impl Kind {
    const fn exit(self) -> ExitCode {
        match self {
            Self::General => ExitCode::Failure,
            Self::Validation => ExitCode::Validation,
            Self::Controller => ExitCode::Controller,
            Self::Core => ExitCode::NoCore,
        }
    }
}

/// One check's outcome.
#[derive(Debug, Serialize)]
pub struct Check {
    /// Stable identifier, from [`CHECKS`].
    pub id: &'static str,
    /// What the check looks at.
    pub title: &'static str,
    /// `pass`, `warn` or `fail`.
    pub status: CheckStatus,
    /// What was found; multi-line when the finding has detail of its own.
    pub detail: String,
    /// What to do about it.
    pub hint: Option<String>,
    /// Machine-readable extras, for the checks that have more to say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// The exit code this check contributes when it fails.
    #[serde(skip)]
    kind: Kind,
}

impl Check {
    fn new(
        id: &'static str,
        title: &'static str,
        status: CheckStatus,
        kind: Kind,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id,
            title,
            status,
            detail: detail.into(),
            hint: None,
            data: None,
            kind,
        }
    }

    fn pass(id: &'static str, title: &'static str, kind: Kind, detail: impl Into<String>) -> Self {
        Self::new(id, title, CheckStatus::Pass, kind, detail)
    }

    fn warn(id: &'static str, title: &'static str, kind: Kind, detail: impl Into<String>) -> Self {
        Self::new(id, title, CheckStatus::Warn, kind, detail)
    }

    fn fail(id: &'static str, title: &'static str, kind: Kind, detail: impl Into<String>) -> Self {
        Self::new(id, title, CheckStatus::Fail, kind, detail)
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// The whole diagnostic report.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    /// Application home.
    pub home: String,
    /// Every check that ran, in order.
    pub checks: Vec<Check>,
    /// Checks that passed.
    pub passed: usize,
    /// Checks that warned.
    pub warnings: usize,
    /// Checks that failed.
    pub failures: usize,
    /// `ok`, `warnings` or `failures`.
    pub verdict: &'static str,
    /// Set when the run stopped early, with the reason.
    pub aborted: Option<String>,
    /// Checks that did not run, because something earlier made them
    /// meaningless. A prefix of [`CHECKS`] is always present in `checks`.
    pub not_checked: Vec<&'static str>,
}

impl DoctorReport {
    fn new(paths: &AppPaths, checks: Vec<Check>, aborted: Option<String>) -> Self {
        let passed = checks
            .iter()
            .filter(|c| c.status == CheckStatus::Pass)
            .count();
        let warnings = checks
            .iter()
            .filter(|c| c.status == CheckStatus::Warn)
            .count();
        let failures = checks
            .iter()
            .filter(|c| c.status == CheckStatus::Fail)
            .count();
        let verdict = if failures > 0 {
            "failures"
        } else if warnings > 0 {
            "warnings"
        } else {
            "ok"
        };
        // Whatever [`CHECKS`] names and the report does not hold never ran;
        // saying so is the difference between "fine" and "unknown".
        let not_checked = CHECKS
            .iter()
            .filter(|id| !checks.iter().any(|check| check.id == **id))
            .copied()
            .collect();
        Self {
            home: paths.home().display().to_string(),
            checks,
            passed,
            warnings,
            failures,
            verdict,
            aborted,
            not_checked,
        }
    }

    /// The code the process exits with: the first failure's code.
    ///
    /// The first rather than the most severe, because the report is read
    /// top-down and the earliest failure is the one the rest follow from.
    #[must_use]
    pub fn exit_code(&self) -> ExitCode {
        self.checks
            .iter()
            .find(|c| c.status == CheckStatus::Fail)
            .map_or(ExitCode::Success, |check| check.kind.exit())
    }
}

impl Report for DoctorReport {
    fn schema(&self) -> &'static str {
        "cvt.doctor.v1"
    }

    fn render(&self, out: Output) -> String {
        let width = self.checks.iter().map(|c| c.title.len()).max().unwrap_or(0);
        let indent = " ".repeat(width + 8);
        let mut text = format!("clash-verge-tui doctor - {}\n\n", self.home);
        for check in &self.checks {
            let tag = match check.status {
                CheckStatus::Pass => out.paint_stdout("\u{1b}[32m", "pass"),
                CheckStatus::Warn => out.paint_stdout("\u{1b}[33m", "warn"),
                CheckStatus::Fail => out.paint_stdout("\u{1b}[31m", "fail"),
            };
            let mut lines = check.detail.lines();
            let head = lines.next().unwrap_or_default();
            let _ = writeln!(
                text,
                "{tag}  {:<width$}  {head}",
                check.title,
                width = width
            );
            for line in lines {
                let _ = writeln!(text, "{indent}{line}");
            }
            if let Some(hint) = &check.hint {
                let _ = writeln!(text, "{indent}hint: {hint}");
            }
        }
        if let Some(reason) = &self.aborted {
            let _ = writeln!(text, "\nstopped early: {reason}");
        }
        if !self.not_checked.is_empty() {
            let _ = writeln!(text, "not checked: {}", self.not_checked.join(", "));
        }
        let _ = writeln!(
            text,
            "\n{} passed, {} warning(s), {} failure(s)",
            self.passed, self.warnings, self.failures
        );
        text.trim_end().to_owned()
    }
}

/// Run every check and report the result.
///
/// # Errors
/// [`Exit`] carrying the code of the first failed check, so a script sees a
/// non-zero status while a human sees the report.
pub async fn run(paths: &AppPaths, out: Output) -> anyhow::Result<()> {
    let report = diagnose(paths, out).await;
    out.emit(&report)?;
    match report.exit_code() {
        ExitCode::Success => Ok(()),
        code => Err(Exit::new(
            code,
            format!(
                "doctor found {} failed check(s): {}",
                report.failures,
                report
                    .checks
                    .iter()
                    .filter(|c| c.status == CheckStatus::Fail)
                    .map(|c| c.id)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .into()),
    }
}

/// Gather the report without deciding what to do about it.
pub async fn diagnose(paths: &AppPaths, out: Output) -> DoctorReport {
    let home = check_home(paths);
    let usable = home.status != CheckStatus::Fail;
    let mut checks = vec![home];

    // A diagnostic must not be the thing that creates the directory it is
    // describing, so an unusable home ends the run here rather than being
    // papered over by the first command that would have created it.
    if !usable {
        return DoctorReport::new(
            paths,
            checks,
            Some(
                "the home directory is not usable, so nothing that needs it was checked".to_owned(),
            ),
        );
    }

    // Opening the service is the first thing that writes to the home, and the
    // check above has already recorded what was there.
    let service = match Service::open(paths.clone()) {
        Ok(service) => service,
        Err(error) => {
            checks.push(
                Check::fail("settings", "settings", Kind::General, format!("{error}"))
                    .with_hint("fix or remove the settings file, then run doctor again"),
            );
            return DoctorReport::new(
                paths,
                checks,
                Some("the settings file could not be read, so nothing that depends on it was checked".to_owned()),
            );
        }
    };
    checks.push(Check::pass(
        "settings",
        "settings",
        Kind::General,
        format!(
            "{} parsed, and the home layout exists",
            service.paths().settings_file().display()
        ),
    ));

    let ctx = Ctx::from_service(service, out);

    let binary = ctx.service().core_binary();
    checks.push(check_core_binary(binary.as_deref()));
    checks.push(check_core_version(&ctx, binary.as_deref()));

    let store = ctx.store();
    checks.push(check_profile_index(&ctx, store.as_ref()));
    let store = store.ok();
    checks.push(check_profile_current(store.as_ref()));
    checks.push(check_chain_documents(ctx.paths(), store.as_ref()));

    let outcome = ctx.service().generate();
    checks.push(check_generation(outcome.as_ref()));
    checks.push(check_core_validate(
        &ctx,
        binary.as_deref(),
        outcome.as_ref(),
    ));

    let endpoint = ctx.service().endpoint().ok().flatten();
    checks.push(check_controller(&ctx, endpoint.as_ref()).await);
    checks.push(check_exposure(endpoint.as_ref()));
    checks.push(check_subscription(store.as_ref()));

    DoctorReport::new(paths, checks, None)
}

/// 1. The home directory exists and can be written to.
///
/// Writability is decided from the permission bits rather than by writing a
/// probe file: a diagnostic must not be the thing that creates the directory
/// it is about to describe.
fn check_home(paths: &AppPaths) -> Check {
    const ID: &str = "home";
    const TITLE: &str = "home directory";
    let home = paths.home();
    if !home.exists() {
        return Check::fail(
            ID,
            TITLE,
            Kind::General,
            format!("{} does not exist", home.display()),
        )
        .with_hint("it is created by the first command that writes; pass --home DIR or set CVT_HOME to use another location");
    }
    if !home.is_dir() {
        return Check::fail(
            ID,
            TITLE,
            Kind::General,
            format!("{} is not a directory", home.display()),
        )
        .with_hint("pass --home DIR or set CVT_HOME to point at one");
    }
    match std::fs::metadata(home) {
        Err(error) => Check::fail(
            ID,
            TITLE,
            Kind::General,
            format!("cannot be inspected: {error}"),
        ),
        // The portable signal. An ACL can still refuse a write that this
        // allows; the first real write is what reports that.
        Ok(metadata) if metadata.permissions().readonly() => {
            Check::fail(ID, TITLE, Kind::General, "is read-only".to_owned())
                .with_hint("make it writable, or point --home at a directory you own")
        }
        Ok(_) => Check::pass(ID, TITLE, Kind::General, "exists and is writable"),
    }
}

/// 3. A core binary can be located.
fn check_core_binary(binary: Option<&Path>) -> Check {
    const ID: &str = "core-binary";
    const TITLE: &str = "core binary";
    match binary {
        Some(path) => Check::pass(ID, TITLE, Kind::Core, format!("using {}", path.display())),
        None => Check::fail(ID, TITLE, Kind::Core, "no mihomo binary found").with_hint(format!(
            "put one in the core directory of the home, or set {CORE_ENV}"
        )),
    }
}

/// 4. The binary's version parses.
///
/// A binary that cannot report a version is not a core this program can drive;
/// finding that out here is cheaper than finding it out at `core start`.
fn check_core_version(ctx: &Ctx, binary: Option<&Path>) -> Check {
    const ID: &str = "core-version";
    const TITLE: &str = "core version";
    let Some(binary) = binary else {
        return Check::warn(ID, TITLE, Kind::Core, "not run: no binary to ask")
            .with_hint(format!("install a core binary, or set {CORE_ENV}"));
    };
    match ctx.service().supervisor().version(binary) {
        Err(error) => Check::fail(
            ID,
            TITLE,
            Kind::Core,
            format!("`{}` -v failed: {error}", binary.display()),
        )
        .with_hint("the file may not be executable, or may be a different program"),
        Ok(text) => match parse_version(&text) {
            Some((major, minor, patch)) => {
                Check::pass(ID, TITLE, Kind::Core, format!("v{major}.{minor}.{patch}"))
                    .with_data(json!({ "reported": text, "semver": [major, minor, patch] }))
            }
            None => Check::fail(ID, TITLE, Kind::Core, format!("no version in `{text}`"))
                .with_hint("the binary did not answer like mihomo; check which program it is"),
        },
    }
}

/// 5. A profile index exists and is readable.
fn check_profile_index(ctx: &Ctx, store: Result<&ProfileStore, &anyhow::Error>) -> Check {
    const ID: &str = "profile-index";
    const TITLE: &str = "profile index";
    let path = ctx.paths().profiles_index();
    match store {
        Err(error) => Check::fail(ID, TITLE, Kind::General, format!("{error}")).with_hint(format!(
            "{} is not valid YAML; fix it or move it aside to start fresh",
            path.display()
        )),
        Ok(store) => {
            if store.items().is_empty() {
                return Check::fail(ID, TITLE, Kind::General, "no profiles are configured").with_hint(
                    "add a subscription: `clash-verge-tui profiles add <url>`, or import an existing \
                     clash-verge-rev home with `clash-verge-tui profiles import <dir>`",
                );
            }
            Check::pass(
                ID,
                TITLE,
                Kind::General,
                format!("{} profile(s) in {}", store.items().len(), path.display()),
            )
        }
    }
}

/// 6. A profile is selected.
fn check_profile_current(store: Option<&ProfileStore>) -> Check {
    const ID: &str = "profile-current";
    const TITLE: &str = "selected profile";
    let Some(store) = store else {
        return Check::warn(
            ID,
            TITLE,
            Kind::General,
            "not run: the index could not be read",
        );
    };
    match store.current() {
        None => Check::fail(ID, TITLE, Kind::General, "no profile is selected").with_hint(
            "pick one with `clash-verge-tui profiles switch <uid>`; `profiles list` prints the uids",
        ),
        Some(item) => item.unsupported_reason().map_or_else(
            || {
                Check::pass(
                    ID,
                    TITLE,
                    Kind::General,
                    format!("`{}` ({}, {})", item.label(), item.uid, item.kind.as_str()),
                )
            },
            |reason| {
                Check::warn(
                    ID,
                    TITLE,
                    Kind::General,
                    format!("`{}` is selected but cannot be used: {reason}", item.label()),
                )
            },
        ),
    }
}

/// 7. Every chain document is readable and parses.
///
/// Script profiles are skipped rather than failed: this program does not run
/// JavaScript, so their contents are not YAML it should try to parse, and the
/// pipeline reports the skip in its own right.
fn check_chain_documents(paths: &AppPaths, store: Option<&ProfileStore>) -> Check {
    const ID: &str = "chain-documents";
    const TITLE: &str = "chain documents";
    let Some(store) = store else {
        return Check::warn(
            ID,
            TITLE,
            Kind::General,
            "not run: the index could not be read",
        );
    };
    let chain = match store.resolve_chain() {
        Ok(chain) => chain,
        Err(error) => {
            return Check::fail(ID, TITLE, Kind::General, format!("{error}")).with_hint(
                "the chain names a profile that does not exist, or names one twice; \
                 `clash-verge-tui profiles chain` prints the resolved order",
            );
        }
    };

    let mut problems: Vec<String> = Vec::new();
    let mut documents: Vec<Value> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for item in &chain {
        if item.unsupported_reason().is_some() {
            skipped.push(item.uid.clone());
            continue;
        }
        let path = cvt_core::profile::store::document_path(paths, item);
        match store.read_document(item) {
            Err(error) => problems.push(format!("{}: {error}", item.uid)),
            Ok(text) => match serde_norway::from_str::<Value>(&text) {
                Ok(_) => documents.push(json!({
                    "uid": item.uid,
                    "name": item.label(),
                    "kind": item.kind.as_str(),
                    "path": path.display().to_string(),
                    "bytes": text.len(),
                })),
                Err(error) => problems.push(format!("{}: {error}", item.uid)),
            },
        }
    }

    let data = json!({ "documents": documents, "skipped": skipped });
    if !problems.is_empty() {
        return Check::fail(
            ID,
            TITLE,
            Kind::General,
            format!(
                "{} document(s) could not be used:\n{}",
                problems.len(),
                problems.join("\n")
            ),
        )
        .with_hint("refresh the profile that owns the file, or fix it by hand")
        .with_data(data);
    }
    let detail = format!(
        "{} document(s) read and parsed{}",
        documents.len(),
        if skipped.is_empty() {
            String::new()
        } else {
            format!(" ({} script profile(s) skipped)", skipped.len())
        }
    );
    let check = Check::pass(ID, TITLE, Kind::General, detail).with_data(data);
    if skipped.is_empty() {
        check
    } else {
        check.with_hint(
            "JavaScript profiles are recognised but not executed; re-express them as `override` profiles",
        )
    }
}

/// 8. The generated configuration validates.
fn check_generation(
    outcome: Result<&cvt_core::enhance::pipeline::Outcome, &cvt_core::Error>,
) -> Check {
    const ID: &str = "config-validation";
    const TITLE: &str = "generated config";
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            return Check::fail(ID, TITLE, Kind::Validation, format!("{error}")).with_hint(
                "nothing can be applied until this succeeds; `clash-verge-tui status` shows the \
                 inputs it used",
            );
        }
    };
    let diagnostics = DiagnosticInfo::collect(&outcome.report);
    let data = serde_json::to_value(&diagnostics).unwrap_or(Value::Null);
    let listing = diagnostics
        .iter()
        .map(|d| {
            format!(
                "{} {} {}{}",
                d.severity,
                d.code,
                d.message,
                d.location
                    .as_deref()
                    .map_or(String::new(), |l| format!(" at {l}"))
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    if outcome.report.errors() > 0 {
        return Check::fail(
            ID,
            TITLE,
            Kind::Validation,
            format!(
                "{} error(s), {} warning(s)\n{listing}",
                outcome.report.errors(),
                outcome.report.warnings()
            ),
        )
        .with_hint("fix the findings above; `clash-verge-tui config validate` lists them again")
        .with_data(data);
    }
    if outcome.report.warnings() > 0 {
        return Check::warn(
            ID,
            TITLE,
            Kind::Validation,
            format!("{} warning(s)\n{listing}", outcome.report.warnings()),
        )
        .with_data(data);
    }
    Check::pass(ID, TITLE, Kind::Validation, outcome.summary()).with_data(data)
}

/// 9. `mihomo -t` accepts the generated configuration.
///
/// The document is written to a temporary file rather than to the runtime
/// path: this command reports, it does not deploy.
fn check_core_validate(
    ctx: &Ctx,
    binary: Option<&Path>,
    outcome: Result<&cvt_core::enhance::pipeline::Outcome, &cvt_core::Error>,
) -> Check {
    const ID: &str = "core-validate";
    const TITLE: &str = "mihomo -t";
    let Some(binary) = binary else {
        return Check::warn(ID, TITLE, Kind::Validation, "not run: no core binary");
    };
    let Ok(outcome) = outcome else {
        return Check::warn(
            ID,
            TITLE,
            Kind::Validation,
            "not run: nothing was generated",
        );
    };

    let temp = std::env::temp_dir().join(format!("cvt-doctor-{}.yaml", std::process::id()));
    if let Err(error) = std::fs::write(&temp, &outcome.yaml) {
        return Check::fail(
            ID,
            TITLE,
            Kind::Validation,
            format!("could not write a check copy: {error}"),
        );
    }
    let result = ctx.service().supervisor().validate_config(binary, &temp);
    // Best effort: leaving a file in the temporary directory is untidy but
    // never a reason to fail a diagnostic.
    let _ = std::fs::remove_file(&temp);

    match result {
        Ok(()) => Check::pass(
            ID,
            TITLE,
            Kind::Validation,
            "the core accepted the document",
        ),
        Err(error) => {
            let detail = match &error {
                cvt_core::Error::ProcessFailed { stderr, .. } => stderr.clone(),
                other => other.to_string(),
            };
            Check::fail(ID, TITLE, Kind::Validation, detail)
                .with_hint("the core's own message is above; `clash-verge-tui config show` prints the document it read")
        }
    }
}

/// 10. The controller is reachable and answers.
async fn check_controller(ctx: &Ctx, endpoint: Option<&Endpoint>) -> Check {
    const ID: &str = "controller";
    const TITLE: &str = "controller";
    let Some(endpoint) = endpoint else {
        return Check::warn(ID, TITLE, Kind::Controller, "no controller is configured")
            .with_hint("add `external-controller: 127.0.0.1:9090` to a profile and apply it");
    };
    if let Err(error) = endpoint.validate() {
        return Check::fail(ID, TITLE, Kind::Controller, format!("{error}"))
            .with_hint("the endpoint has to be `host:port`, a unix socket, or a named pipe");
    }
    let client = match ctx.client() {
        Ok(client) => client,
        Err(error) => {
            return Check::fail(ID, TITLE, Kind::Controller, format!("{error}"));
        }
    };
    match client.probe().await {
        Ok(capabilities) => {
            let summary = capabilities
                .summary()
                .iter()
                .map(|(label, value)| format!("{label}: {value}"))
                .collect::<Vec<_>>()
                .join(", ");
            Check::pass(
                ID,
                TITLE,
                Kind::Controller,
                format!("{} answers ({summary})", endpoint.describe()),
            )
            .with_data(json!({
                "endpoint": endpoint.describe(),
                "version": capabilities.version,
                "rules_disable": capabilities.rules_disable,
                "configs_write": capabilities.configs_write,
                "debug": capabilities.debug,
                "upgrade": capabilities.upgrade,
            }))
        }
        Err(error) => Check::fail(
            ID,
            TITLE,
            Kind::Controller,
            format!("{} did not answer: {}", endpoint.describe(), error.short()),
        )
        .with_hint(
            "start the core with `clash-verge-tui core start`, or check the port it listens on",
        ),
    }
}

/// 11. A non-loopback controller with no secret is a real security problem.
fn check_exposure(endpoint: Option<&Endpoint>) -> Check {
    const ID: &str = "controller-exposure";
    const TITLE: &str = "controller exposure";
    let Some(endpoint) = endpoint else {
        return Check::warn(
            ID,
            TITLE,
            Kind::General,
            "not run: no controller is configured",
        );
    };
    if endpoint.is_loopback() {
        return Check::pass(
            ID,
            TITLE,
            Kind::General,
            format!("{} is loopback-only", endpoint.describe()),
        );
    }
    if endpoint.is_authenticated() {
        return Check::warn(
            ID,
            TITLE,
            Kind::General,
            format!(
                "{} is reachable from the network, protected by a secret",
                endpoint.describe()
            ),
        )
        .with_hint("bind it to 127.0.0.1 if you do not need remote access");
    }
    Check::warn(
        ID,
        TITLE,
        Kind::General,
        format!(
            "{} is reachable from the network and has no secret",
            endpoint.describe()
        ),
    )
    .with_hint(
        "anyone who can reach that port controls the core: set `secret` in the configuration, or \
         bind the controller to 127.0.0.1",
    )
}

/// 12. Subscriptions that are due and quotas that are running out.
fn check_subscription(store: Option<&ProfileStore>) -> Check {
    const ID: &str = "subscription";
    const TITLE: &str = "subscriptions";
    let Some(store) = store else {
        return Check::warn(
            ID,
            TITLE,
            Kind::General,
            "not run: the index could not be read",
        );
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let remote: Vec<_> = store.items().iter().filter(|i| i.is_remote()).collect();
    if remote.is_empty() {
        return Check::pass(ID, TITLE, Kind::General, "no remote profiles");
    }
    let entries: Vec<Value> = remote
        .iter()
        .map(|item| {
            json!({
                "uid": item.uid,
                "name": item.label(),
                "updated": item.updated,
                "due": is_due(item, now),
                "used_bytes": item.extra.used(),
                "total_bytes": item.extra.total,
                "expire": item.extra.expire,
                "interval_minutes": item.option.effective_interval(),
            })
        })
        .collect();
    let due: Vec<_> = remote
        .iter()
        .filter(|item| is_due(item, now))
        .map(|item| item.label().to_owned())
        .collect();
    let data = json!({ "subscriptions": entries });
    if due.is_empty() {
        return Check::pass(
            ID,
            TITLE,
            Kind::General,
            format!("{} subscription(s) are current", remote.len()),
        )
        .with_data(data);
    }
    Check::warn(
        ID,
        TITLE,
        Kind::General,
        format!(
            "{} of {} subscription(s) are due: {}",
            due.len(),
            remote.len(),
            due.join(", ")
        ),
    )
    .with_hint("refresh them with `clash-verge-tui profiles update --all-due`")
    .with_data(data)
}

/// The `X.Y.Z` triple in a `mihomo -v` line.
///
/// `mihomo -v` prints `Mihomo Meta v1.19.31 linux amd64`, and a proxy that
/// answers with something else is worth failing on rather than trusting.
#[must_use]
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    for token in text.split_whitespace() {
        let candidate = token.trim_start_matches('v');
        let mut parts = candidate.split('.');
        let (Some(major), Some(minor)) = (parts.next(), parts.next()) else {
            continue;
        };
        if let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) {
            let patch = parts
                .next()
                .and_then(|p| p.parse::<u32>().ok())
                .unwrap_or(0);
            return Some((major, minor, patch));
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cvt_core::AppPaths;
    use tempfile::TempDir;

    fn quiet() -> Output {
        Output::new(false, 0, false)
    }

    #[test]
    fn a_version_line_is_parsed_the_way_the_core_prints_it() {
        assert_eq!(
            parse_version("Mihomo Meta v1.19.31 linux amd64 with go1.24"),
            Some((1, 19, 31))
        );
        assert_eq!(parse_version("v1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("Clash.Meta 1.18.0"), Some((1, 18, 0)));
        assert_eq!(parse_version("no digits here"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(
            parse_version("/usr/bin/mihomo"),
            None,
            "a path is not a version"
        );
    }

    #[tokio::test]
    async fn every_check_runs_in_the_documented_order() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        let report = diagnose(&paths, quiet()).await;
        let ids: Vec<&str> = report.checks.iter().map(|c| c.id).collect();
        assert_eq!(ids, CHECKS, "the order is the contract");
        assert!(
            report.aborted.is_none(),
            "nothing should have stopped the run"
        );
    }

    #[tokio::test]
    async fn a_home_that_does_not_exist_fails_before_anything_creates_it() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("never-created");
        let paths = AppPaths::new(&home);
        let report = diagnose(&paths, quiet()).await;
        assert!(
            !home.exists(),
            "doctor must not create the home it reports on"
        );
        assert_eq!(report.checks[0].id, "home");
        assert_eq!(report.checks[0].status, CheckStatus::Fail);
        assert_eq!(report.exit_code(), ExitCode::Failure);
        assert!(
            report.checks[0]
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("--home")),
            "{:?}",
            report.checks[0].hint
        );
    }

    #[tokio::test]
    async fn a_missing_core_binary_is_exit_code_five() {
        let dir = TempDir::new().unwrap();
        let report = diagnose(&AppPaths::new(dir.path()), quiet()).await;
        let binary = report
            .checks
            .iter()
            .find(|c| c.id == "core-binary")
            .unwrap();
        assert_eq!(binary.status, CheckStatus::Fail);
        assert_eq!(report.exit_code(), ExitCode::NoCore);
        let version = report
            .checks
            .iter()
            .find(|c| c.id == "core-version")
            .unwrap();
        assert_eq!(
            version.status,
            CheckStatus::Warn,
            "a skipped check is not a failure"
        );
    }

    #[tokio::test]
    async fn an_unusable_settings_file_stops_the_run_and_says_so() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.settings_file(), "core: [broken\n").unwrap();
        let report = diagnose(&paths, quiet()).await;
        assert_eq!(report.checks.len(), 2, "home then settings");
        assert_eq!(report.checks[1].id, "settings");
        assert_eq!(report.checks[1].status, CheckStatus::Fail);
        assert!(report.aborted.is_some());
        assert_eq!(report.verdict, "failures");
    }

    #[tokio::test]
    async fn a_configured_controller_that_nothing_listens_to_is_a_controller_failure() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        let mut store = ProfileStore::load(&paths).unwrap();
        let uid = store.add(cvt_core::profile::item::PrfItem::local("L1", "local"));
        let item = store.get(&uid).unwrap().clone();
        // Port 1 is never bound, so the probe fails as fast as a refusal.
        store
            .write_document(
                &item,
                "external-controller: 127.0.0.1:1\nmixed-port: 7890\nproxies:\n  - { name: a, type: vless, server: 1.2.3.4, port: 1, uuid: u }\nproxy-groups:\n  - { name: G, type: select, proxies: [a] }\nrules:\n  - MATCH,G\n",
            )
            .unwrap();
        store.set_current(&uid).unwrap();
        store.save().unwrap();
        // A core binary that answers `-v` and accepts any document, so the
        // checks before the controller's can pass and the exit code is the
        // controller's own.
        install_fake_core(&paths, "exit 0");

        let report = diagnose(&paths, quiet()).await;
        let controller = report.checks.iter().find(|c| c.id == "controller").unwrap();
        assert_eq!(
            controller.status,
            CheckStatus::Fail,
            "detail: {}",
            controller.detail
        );
        assert_eq!(report.exit_code(), ExitCode::Controller);
        let exposure = report
            .checks
            .iter()
            .find(|c| c.id == "controller-exposure")
            .unwrap();
        assert_eq!(exposure.status, CheckStatus::Pass, "127.0.0.1 is loopback");
        let validate = report
            .checks
            .iter()
            .find(|c| c.id == "core-validate")
            .unwrap();
        assert_eq!(
            validate.status,
            CheckStatus::Pass,
            "detail: {}",
            validate.detail
        );
    }

    /// A stand-in for `mihomo`: answers `-v` with a real version line, and
    /// whatever `-t` should do is the script's second line.
    #[cfg(unix)]
    fn install_fake_core(paths: &AppPaths, on_test: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let binary = paths.core_dir().join("mihomo");
        std::fs::create_dir_all(paths.core_dir()).unwrap();
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  -v) echo \"Mihomo Meta v1.19.31 linux amd64\" ;;\n  -t) {on_test} ;;\nesac\nexit 0\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(not(unix))]
    fn install_fake_core(_paths: &AppPaths, _on_test: &str) {}

    #[cfg(unix)]
    #[tokio::test]
    async fn a_core_that_rejects_the_document_is_a_validation_failure() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        let mut store = ProfileStore::load(&paths).unwrap();
        let uid = store.add(cvt_core::profile::item::PrfItem::local("L1", "local"));
        let item = store.get(&uid).unwrap().clone();
        store
            .write_document(
                &item,
                "external-controller: 127.0.0.1:1\nproxies:\n  - { name: a, type: vless, server: 1.2.3.4, port: 1, uuid: u }\nrules:\n  - MATCH,a\n",
            )
            .unwrap();
        store.set_current(&uid).unwrap();
        store.save().unwrap();
        install_fake_core(&paths, "echo 'parse error: bad rule' >&2; exit 1");

        let report = diagnose(&paths, quiet()).await;
        let validate = report
            .checks
            .iter()
            .find(|c| c.id == "core-validate")
            .unwrap();
        assert_eq!(validate.status, CheckStatus::Fail);
        assert!(
            validate.detail.contains("parse error"),
            "the core's own message is the useful half: {}",
            validate.detail
        );
        assert_eq!(report.exit_code(), ExitCode::Validation);
    }

    #[test]
    fn a_non_loopback_controller_without_a_secret_warns_about_it() {
        let open = check_exposure(Some(&Endpoint::tcp("0.0.0.0:9090", None)));
        assert_eq!(open.status, CheckStatus::Warn);
        assert!(open.detail.contains("no secret"), "{}", open.detail);
        assert!(open.hint.is_some());

        let guarded = check_exposure(Some(&Endpoint::tcp("0.0.0.0:9090", Some("s".into()))));
        assert_eq!(guarded.status, CheckStatus::Warn);
        assert!(guarded.detail.contains("protected by a secret"));

        let local = check_exposure(Some(&Endpoint::tcp("127.0.0.1:9090", None)));
        assert_eq!(local.status, CheckStatus::Pass);
    }

    #[test]
    fn the_first_failure_decides_the_exit_code() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        // Home is fine; the core binary is the first real failure.
        let report = DoctorReport::new(
            &paths,
            vec![
                Check::pass("home", "home directory", Kind::General, "ok"),
                Check::fail("core-binary", "core binary", Kind::Core, "none"),
                Check::fail("controller", "controller", Kind::Controller, "unreachable"),
            ],
            None,
        );
        assert_eq!(report.exit_code(), ExitCode::NoCore);
        assert_eq!(report.failures, 2);
        assert_eq!(report.verdict, "failures");
    }

    #[test]
    fn the_json_shape_names_every_check() {
        let dir = TempDir::new().unwrap();
        let report = DoctorReport::new(
            &AppPaths::new(dir.path()),
            vec![
                Check::fail("home", "home directory", Kind::General, "gone")
                    .with_hint("make it")
                    .with_data(json!({"x": 1})),
            ],
            None,
        );
        let value = crate::output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.doctor.v1"));
        assert_eq!(value["checks"][0]["id"], serde_json::json!("home"));
        assert_eq!(value["checks"][0]["status"], serde_json::json!("fail"));
        assert_eq!(value["checks"][0]["data"]["x"], serde_json::json!(1));
        assert_eq!(value["verdict"], serde_json::json!("failures"));
        assert!(
            value["checks"][0].get("kind").is_none(),
            "internal fields stay out"
        );
    }

    #[test]
    fn a_report_renders_one_line_per_check_plus_an_indented_hint() {
        let dir = TempDir::new().unwrap();
        let report = DoctorReport::new(
            &AppPaths::new(dir.path()),
            vec![
                Check::pass("home", "home directory", Kind::General, "fine"),
                Check::fail("core-binary", "core binary", Kind::Core, "none")
                    .with_hint("install one"),
            ],
            None,
        );
        let text = report.render(quiet());
        assert!(text.contains("pass  home directory"), "{text}");
        assert!(text.contains("fail  core binary"), "{text}");
        assert!(text.contains("hint: install one"), "{text}");
        assert!(
            text.contains("1 passed, 0 warning(s), 1 failure(s)"),
            "{text}"
        );
    }
}
