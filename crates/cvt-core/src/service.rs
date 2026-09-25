//! The application facade.
//!
//! Everything above this line is a library: a config document, a merge engine,
//! a store, an API client, a process supervisor. [`Service`] is where those
//! become *operations a user asks for* — "apply this profile", "start the
//! core", "test these nodes" — and it is the only type the TUI and the CLI
//! need to hold.
//!
//! # Why the ordering logic lives here
//!
//! Applying a configuration has a decision tree that is easy to get subtly
//! wrong and expensive to get wrong in production:
//!
//! 1. generate and validate; refuse to touch anything if the result is broken;
//! 2. hand the document to the core over the API, which applies most changes
//!    without dropping connections;
//! 3. only if that fails, restart the process;
//! 4. if the core then fails to come up, restore the previous snapshot and
//!    start again — because "I edited my rules and now my network is gone" is
//!    the worst outcome this program can produce, and the snapshot is already
//!    on disk.
//!
//! Steps 2 to 4 are [`Service::apply_with`], and the whole sequence is
//! exercisable in tests through a small injection point rather than a real
//! core.

use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::enhance::pipeline::{Outcome, Pipeline};
use crate::error::{Error, Result};
use crate::mihomo::client::Client;
use crate::mihomo::endpoint::Endpoint;
use crate::mihomo::supervisor::{CoreStatus, Supervisor};
use crate::model::config::Config;
use crate::paths::AppPaths;
use crate::profile::store::ProfileStore;
use crate::settings::Settings;
/// How long one look at one group may take, how long the whole replay may
/// take, and how often to look again.
///
/// Three attempts got this wrong in three different ways, and the shape of the
/// answer is the third: a deadline per group multiplies (twenty groups that are
/// gone cost twenty waits); a single deadline for the whole replay hands
/// everything to the first group that is gone; and waiting on each group *in
/// turn* inside that deadline does the same thing more slowly — six groups the
/// subscription has removed cost six waits, and the seventh choice, the one
/// that would have worked, is never reached.
///
/// So nothing waits on a group. The ones that are there are replayed first, and
/// the rest are polled together until they answer or the total runs out, which
/// makes a group that is gone cost one read per round rather than a share of
/// the budget.
const REPLAY_PER_GROUP: std::time::Duration = std::time::Duration::from_millis(250);
const REPLAY_TOTAL: std::time::Duration = std::time::Duration::from_millis(2000);
const REPLAY_STEP: std::time::Duration = std::time::Duration::from_millis(25);

/// How long this group may take, given how much of the total is left.
fn group_budget(overall: std::time::Instant, limit: std::time::Duration) -> std::time::Instant {
    (std::time::Instant::now() + limit).min(overall)
}

/// Point a group at a member and make sure it took.
async fn replay_one(
    client: &Client,
    name: &str,
    member: &str,
    deadline: std::time::Instant,
) -> bool {
    if !select_within(client, name, member, deadline).await {
        return false;
    }
    confirm_selection(client, name, member, deadline).await
}

/// Choose a member, without letting the call outlive the budget.
///
/// The reads were given a deadline and this was not, which is the same mistake
/// one call further down: a core that answers `GET /group/…` and never answers
/// the `PUT` costs the *client's* timeout — five seconds from the settings —
/// which is not the replay's budget and cannot be enforced from here.
async fn select_within(
    client: &Client,
    name: &str,
    member: &str,
    deadline: std::time::Instant,
) -> bool {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return false;
    }
    matches!(
        tokio::time::timeout(left, client.select(name, member)).await,
        Ok(Ok(()))
    )
}

/// Read a group, without letting one slow request outlive the budget.
///
/// The client has its own timeout, which is the *settings'* and can be seconds;
/// a deadline this function cannot enforce is a deadline in name only.
async fn read_group(
    client: &Client,
    group: &str,
    deadline: std::time::Instant,
) -> Option<crate::mihomo::types::ProxyView> {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return None;
    }
    tokio::time::timeout(left, client.group(group))
        .await
        .ok()?
        .ok()
}

/// Wait until the core reports the member that was asked for.
///
/// A `select` group reports the choice as `now`. A `url-test` or `fallback`
/// group *pins* it instead and reports `fixed`, keeping `now` for whatever the
/// test last picked — so checking only `now` reported a pin that had taken as a
/// failure, which is how the first version of this managed to apply a choice
/// and count zero. Either field is the choice having taken.
async fn confirm_selection(
    client: &Client,
    group: &str,
    member: &str,
    deadline: std::time::Instant,
) -> bool {
    loop {
        match read_group(client, group, deadline).await {
            Some(view)
                if view.now.as_deref() == Some(member) || view.fixed.as_deref() == Some(member) =>
            {
                return true;
            }
            // Not there at all: this document does not have the group, and the
            // next look would fail the same way.
            None if std::time::Instant::now() >= deadline => return false,
            _ => {}
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(REPLAY_STEP).await;
    }
}

/// How many backups to keep before the oldest is deleted.
///
/// Five is a working week at one a day, which is the cadence a person actually
/// keeps; a backup taken before every experiment would fill a disk with
/// versions of the same file.
pub const BACKUP_LIMIT: usize = 5;

/// One directory of saved state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    /// Where it is.
    pub path: PathBuf,
    /// Unix timestamp of when it was taken.
    pub created: i64,
    /// Which backup of that second it was, `1` for the first.
    ///
    /// Carried separately because two backups taken in the same second share a
    /// timestamp, and sorting on the timestamp alone leaves their order to
    /// whatever `read_dir` produced — so the same six backups pruned to
    /// different survivors depending on the order they were made in.
    pub sequence: u32,
    /// How many entries it holds, for a report.
    pub items: usize,
}

impl Backup {
    /// The name, which is the timestamp.
    #[must_use]
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Whether a directory is one of this program's backups.
///
/// The four things [`copy_state`] writes, any of which is evidence: a home that
/// has never saved its settings or had an index still produces a backup, and a
/// check demanding one particular file refuses the program's own output.
fn looks_like_a_backup(dir: &Path) -> bool {
    // `symlink_metadata`, not `exists`: that follows the link, so a directory
    // whose entries are all links was admitted as a backup and then every
    // copier skipped everything it held — a restore that did nothing at all and
    // reported success. The admission test and the copiers have to agree about
    // what a link means, and they now both say "not this program's".
    ["cvt.yaml", "profiles.yaml", "profiles", "overrides"]
        .iter()
        .any(|name| {
            std::fs::symlink_metadata(dir.join(name))
                .is_ok_and(|meta| !meta.file_type().is_symlink())
        })
}

/// Copy the state a user would have to recreate by hand.
///
/// Two directories rather than an [`AppPaths`] and a directory, because the
/// same function runs in both directions: a backup reads the home and writes a
/// backup directory, a restore reads a backup directory and writes the home.
/// One shape for both directions is what makes it obvious that they agree about
/// which files matter.
fn copy_state(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| Error::io(to, e))?;
    for name in ["cvt.yaml", "profiles.yaml"] {
        let source = from.join(name);
        // A *link* where a scalar file goes is skipped, exactly as `copy_dir`
        // skips a directory reached through one: reading it copies the
        // contents of a file outside the home into the backup, which is the
        // opposite of the omission the skip was written for and worse.
        if std::fs::symlink_metadata(&source).is_ok_and(|meta| meta.file_type().is_symlink()) {
            tracing::warn!(
                path = %source.display(),
                "skipping a file reached through a symbolic link"
            );
            continue;
        }
        if source.is_file() {
            let destination = to.join(name);
            copy_file(&source, &destination)?;
        }
    }
    for name in ["profiles", "overrides"] {
        copy_dir(&from.join(name), &to.join(name))?;
    }
    Ok(())
}

/// Whether two paths name the same file, by the identity the filesystem gives
/// it rather than by the path.
///
/// A hard link is one file with two names, and `std::fs::copy(x, y)` where `x`
/// and `y` are those two names truncates the file before reading it — the copy
/// then reports success having written nothing. Comparing canonical paths
/// misses it, because the paths really are different.
#[cfg(unix)]
fn is_same_file(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::metadata(left).ok(), std::fs::metadata(right).ok()) {
        (Some(a), Some(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// The portable fallback: the most a path alone can say is whether it is the
/// same path.
#[cfg(not(unix))]
fn is_same_file(left: &Path, right: &Path) -> bool {
    let same = std::fs::canonicalize(left).ok();
    same.is_some() && same == std::fs::canonicalize(right).ok()
}

/// Everything a copy would write, checked before any of it is written.
///
/// A restore that refuses halfway has already replaced part of the home, and
/// the state it leaves is two configurations at once with nothing saying so.
/// The refusal has to come first, which means walking the whole tree — the
/// sources *and* the destinations — before the first byte moves.
///
/// # Errors
/// [`Error::InvalidValue`] naming the first destination that cannot be written.
fn check_copy(from: &Path, to: &Path) -> Result<()> {
    for name in ["cvt.yaml", "profiles.yaml"] {
        let source = from.join(name);
        // A link *inside the backup* is refused rather than followed: the
        // restore would copy the contents of a file outside the backup into the
        // home, which is the escape `check_destination` refuses at the other
        // end of the same copy.
        if std::fs::symlink_metadata(&source).is_ok_and(|meta| meta.file_type().is_symlink()) {
            // Skipped, not refused, and the same way round as `copy_dir`: a
            // restore from a backup holding a link puts back everything else.
            continue;
        }
        if source.is_file() {
            check_destination(&source, &to.join(name))?;
        }
    }
    for name in ["profiles", "overrides"] {
        let source = from.join(name);
        if !source.is_dir() {
            continue;
        }
        check_directory_destination(&to.join(name))?;
        let Ok(entries) = std::fs::read_dir(&source) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                check_destination(
                    &source.join(entry.file_name()),
                    &to.join(name).join(entry.file_name()),
                )?;
            }
        }
    }
    Ok(())
}

/// Whether a directory destination can be written.
///
/// A directory has its own two answers: a *symlink* would be followed, putting
/// everything outside the home, and something that is not a directory at all —
/// a file where `profiles/` belongs — cannot hold what is being copied into it.
fn check_directory_destination(path: &Path) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is a symbolic link; writing through it would put the files \
                 outside the home. Remove the link, or restore by hand.",
                path.display()
            ),
        ));
    }
    if !meta.is_dir() {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is not a directory, so the files that belong in it cannot be \
                 restored there",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Whether one file destination can be written, and why not when it cannot.
///
/// Four answers, and the first three are the ones this function exists for:
/// a *symlink* would be followed, putting the file outside the home; a *fifo*
/// blocks `std::fs::copy`'s open forever, so the command hangs with no output
/// rather than failing; and a *hard link* — one inode, two names — is a second
/// name for a file this program did not create, and truncating it edits that
/// file. Everything else that is a regular file, or is not there, is fine.
fn check_destination(source: &Path, path: &Path) -> Result<()> {
    // A destination that already *is* the source's file needs nothing, and a
    // link pointing at the source is that: refusing it would refuse a restore
    // that has nothing to do. The order matters and got it wrong once.
    if is_same_file(source, path) {
        return Ok(());
    }
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is a symbolic link; writing through it would put the files \
                 outside the home. Remove the link, or restore by hand.",
                path.display()
            ),
        ));
    }
    if !meta.is_file() {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is not a regular file, so a copy into it would block or fail; \
                 remove it, or restore by hand",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Copy one file, refusing to copy it onto itself.
///
/// The guard is here and not only at the entry points, because this is the
/// function that would do the truncating and a caller added later would not know
/// to check. `std::fs::copy` opens the destination for writing before it reads
/// the source, so a file copied onto itself comes back empty and the call
/// reports success.
fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    // The same *file*, not merely the same path. Canonicalising compares names,
    // and two names for one inode — a hard link, which is what `cp -al` leaves
    // behind — are not the same name. `std::fs::copy` then truncates the file
    // before reading it, and a restore emptied the document it was asked to put
    // back.
    if is_same_file(source, destination) {
        return Ok(());
    }
    check_destination(source, destination)?;
    // A destination that is a *hard link* is replaced rather than written
    // through. `std::fs::copy` truncates the inode, so a document that is a
    // second name for a file outside the home had that file edited — the same
    // escape the symlink guard refuses, one `symlink_metadata` further down.
    // Unlinking the name first breaks the link: the copy gets a fresh inode,
    // and the other name keeps what it had. A refusal would be defensible, but
    // a home whose documents were hard-linked by `cp -al` should still be
    // restorable, and this is the answer that allows both.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if std::fs::metadata(destination).is_ok_and(|meta| meta.nlink() > 1) {
            std::fs::remove_file(destination).map_err(|e| Error::io(destination, e))?;
        }
    }
    // A destination that is a *symlink* is refused rather than followed, with
    // one exception that comes before this: a link pointing at the *source*
    // returns early above, because the destination already is the source's
    // file and there is nothing to do. That is deliberate rather than an
    // oversight — the alternative is refusing a restore that would have
    // succeeded — but it means this comment is about every other link.
    //
    // The reason for the refusal: `std::fs::copy` opens the destination for
    // writing, which follows the link, so a home whose `profiles/L1.yaml` is a
    // link into somebody's dotfiles had a restore overwrite that file, outside
    // the home, silently. The link is the user's arrangement and is not this
    // function's to replace.
    if std::fs::symlink_metadata(destination).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is a symbolic link; a restore would write through it to \
                 wherever it points. Remove the link, or restore by hand.",
                destination.display()
            ),
        ));
    }
    std::fs::copy(source, destination).map_err(|e| Error::io(destination, e))?;
    Ok(())
}

/// Copy a directory of files, skipping anything that is not a regular file.
///
/// Written out rather than pulled from a crate: these are two directories of
/// small text files, and the cases worth stating — a missing directory, a
/// symlink — are clearer here than in a configuration.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    // A directory reached *through a symlink* is not this home's. Reading one
    // copies files from wherever the link points — the test that found this
    // put a `secret.yaml` in somebody else's directory and it arrived in the
    // backup — and writing one puts files there, which is a restore escaping
    // the home.
    //
    // The two directions answer differently and the asymmetry is deliberate. A
    // source that is a link is *skipped*: a backup that quietly omits something
    // is recoverable, and refusing would mean a user with `profiles/` symlinked
    // to another disk can never take one at all. A destination that is a link
    // is *refused*: a restore that silently writes outside the home is not
    // recoverable, and one that silently does nothing would be a lie.
    if std::fs::symlink_metadata(from).is_ok_and(|meta| meta.file_type().is_symlink()) {
        tracing::warn!(
            path = %from.display(),
            "skipping a directory reached through a symbolic link"
        );
        return Ok(());
    }
    if std::fs::symlink_metadata(to).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(Error::invalid(
            "backup",
            format!(
                "{} is a symbolic link; writing through it would put the files \
                 outside the home. Remove the link, or restore by hand.",
                to.display()
            ),
        ));
    }
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to).map_err(|e| Error::io(to, e))?;
    let entries = std::fs::read_dir(from).map_err(|e| Error::io(from, e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        // Only regular files. A symlink here is not something this program
        // writes, and following one would copy a file from wherever the link
        // points — including out of the home entirely.
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        copy_file(&path, &to.join(name))?;
    }
    Ok(())
}

/// How a configuration change should reach the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadMode {
    /// Try the API first, restarting only if that fails. The default.
    Auto,
    /// Only ever use the API; a failure is reported rather than worked around.
    HotReload,
    /// Always restart the process.
    Restart,
}

impl ReloadMode {
    /// Short label for the status bar.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::HotReload => "hot reload",
            Self::Restart => "restart",
        }
    }
}

/// What actually happened when a configuration was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// The core accepted the document over the API.
    HotReloaded,
    /// The process was restarted.
    Restarted {
        /// The new process id.
        pid: u32,
    },
    /// The document failed to load, so the previous snapshot was restored and
    /// the core was started again with it.
    RolledBack {
        /// Why the new document was rejected.
        reason: String,
        /// The snapshot that was restored.
        snapshot: PathBuf,
    },
}

impl ReloadOutcome {
    /// `true` when the core is running the requested configuration.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::HotReloaded | Self::Restarted { .. })
    }

    /// One-line description for a status message.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::HotReloaded => "reloaded without a restart".to_owned(),
            Self::Restarted { pid } => format!("restarted (pid {pid})"),
            Self::RolledBack { reason, .. } => {
                format!("rejected ({reason}); restored the previous configuration")
            }
        }
    }
}

/// The result of a full apply.
#[derive(Debug, Clone)]
pub struct ApplyReport {
    /// What the pipeline produced.
    pub outcome: Outcome,
    /// Whether the change reached the core, and how.
    pub reload: Option<ReloadOutcome>,
    /// `true` when the generated document was written to disk.
    pub written: bool,
    /// How many remembered node choices were replayed onto the core.
    ///
    /// Zero is the ordinary answer for a profile nobody has chosen a node in.
    /// It is reported rather than done silently because a *choice* is
    /// something a user made, and one that could not be replayed — a group the
    /// subscription renamed, a node it dropped — is worth being able to see.
    pub selections_restored: usize,
}

impl ApplyReport {
    /// `true` when the configuration is live.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.reload.as_ref().is_some_and(ReloadOutcome::succeeded)
    }
}

/// The headless application.
#[derive(Debug, Clone)]
pub struct Service {
    paths: AppPaths,
    settings: Settings,
}

impl Service {
    /// Open the application home, creating directories and loading settings.
    ///
    /// # Errors
    /// [`Error::Io`] when the home cannot be created, [`Error::Parse`] when
    /// the settings file is malformed.
    pub fn open(paths: AppPaths) -> Result<Self> {
        paths.ensure_dirs()?;
        let settings = Settings::load(&paths)?;
        Ok(Self { paths, settings })
    }

    /// Open an already-resolved service, skipping validation.
    ///
    /// # Errors
    /// As [`Service::open`].
    pub fn open_default() -> Result<Self> {
        Self::open(AppPaths::resolve(None)?)
    }

    /// The application home.
    #[must_use]
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Current settings.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Replace the settings in memory. Call [`Service::save_settings`] to
    /// persist them.
    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    /// Persist the settings, validating first.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] when they are invalid, otherwise propagates the
    /// write failure.
    pub fn save_settings(&self) -> Result<()> {
        self.settings.save(&self.paths)
    }

    /// Load the profile store.
    ///
    /// # Errors
    /// [`Error::Parse`] when the index is malformed.
    pub fn store(&self) -> Result<ProfileStore> {
        ProfileStore::load(&self.paths)
    }

    /// The process supervisor.
    #[must_use]
    pub fn supervisor(&self) -> Supervisor {
        Supervisor::new(self.paths.clone())
    }

    /// The pipeline that generates runtime configurations.
    #[must_use]
    pub fn pipeline(&self) -> Pipeline {
        Pipeline::new(self.paths.clone()).with_control_plane(
            self.settings.core.external_controller.as_deref(),
            self.settings.core.secret.as_deref(),
        )
    }

    /// Where the controller is, as far as can be determined.
    ///
    /// Reads, in order: the generated runtime configuration, then the profile
    /// that is currently selected. A generated file is authoritative because
    /// it is what the running core was actually launched with.
    ///
    /// # Errors
    /// [`Error::Io`] or [`Error::Parse`] when the runtime configuration exists
    /// but cannot be read.
    pub fn endpoint(&self) -> Result<Option<Endpoint>> {
        let runtime = self.paths.runtime_config();
        if runtime.is_file() {
            let text = self.paths.read(&runtime)?;
            let config = Config::from_yaml(&text)?;
            if let Some(endpoint) = Endpoint::from_config(&config) {
                return Ok(Some(endpoint));
            }
        }
        let store = self.store()?;
        let Some(current) = store.current() else {
            return Ok(None);
        };
        let Ok(text) = store.read_document(current) else {
            return Ok(None);
        };
        Ok(Config::from_yaml(&text)
            .ok()
            .and_then(|c| Endpoint::from_config(&c)))
    }

    /// A client for the controller.
    ///
    /// # Errors
    /// [`Error::MissingField`] when no configuration declares a controller,
    /// so the caller can tell "not configured yet" from "unreachable".
    pub fn client(&self) -> Result<Client> {
        let endpoint = self.endpoint()?.ok_or_else(|| Error::MissingField {
            uid: "-".to_owned(),
            field: "external-controller",
        })?;
        Client::with_timeout(
            endpoint,
            std::time::Duration::from_millis(self.settings.ui.refresh_ms.max(1000) * 5),
        )
    }

    /// Generate a runtime configuration without writing anything.
    ///
    /// # Errors
    /// Propagates pipeline failures.
    pub fn generate(&self) -> Result<Outcome> {
        let store = self.store()?;
        self.pipeline().generate(&store)
    }

    /// Report the core's running state.
    #[must_use]
    pub fn core_status(&self) -> CoreStatus {
        self.supervisor().status()
    }

    /// Locate the core binary, honouring the configured override.
    #[must_use]
    pub fn core_binary(&self) -> Option<PathBuf> {
        self.supervisor()
            .locate(self.settings.core.binary.as_deref())
    }

    /// Start the core using the generated configuration.
    ///
    /// The configuration is validated with `mihomo -t` first: a crash loop is
    /// far harder to diagnose than one error message.
    ///
    /// # Errors
    /// [`Error::CoreUnavailable`] when nothing to run can be found,
    /// [`Error::Validation`] when the document has errors, and
    /// [`Error::ProcessFailed`] when validation or the launch fails.
    pub fn start_core(&self) -> Result<u32> {
        let supervisor = self.supervisor();
        let binary = self.core_binary().ok_or_else(|| Error::CoreUnavailable {
            reason: format!(
                "no mihomo binary found; put one at {} or set {}",
                self.paths.core_dir().join("mihomo").display(),
                crate::mihomo::supervisor::CORE_ENV
            ),
        })?;
        let config = self.paths.runtime_config();
        if !config.is_file() {
            return Err(Error::invalid(
                "runtime config",
                "no configuration has been generated yet; apply a profile first",
            ));
        }
        let text = self.paths.read(&config)?;
        let parsed = Config::from_yaml(&text)?;
        let report = crate::validate::check(&parsed);
        if !report.is_ok() {
            return Err(Error::Validation {
                problems: report.errors_iter().map(|d| d.message.clone()).collect(),
            });
        }
        // Before the rotation, not after: `Supervisor::start` is where the
        // "already running" check lives, and rotating first meant a *refused*
        // start moved the log of the core that is still running — which then
        // kept writing into `.1`, with nothing left to create `core.log`,
        // because the start that would have opened it never happened.
        if let CoreStatus::Running { pid, .. } = supervisor.status() {
            return Err(Error::Unsupported(format!(
                "the core is already running as pid {pid}; stop it first"
            )));
        }
        supervisor.validate_config(&binary, &config)?;
        // Logs are rotated here or never: the child holds its log open for as
        // long as it runs, so this is the only moment either file can be moved
        // without a live process writing into a file nobody will read.
        self.rotate_logs(&supervisor);
        supervisor.start(&binary, &config)
    }

    /// Rotate and prune both logs, best effort.
    ///
    /// Deliberately not fallible: a user who cannot rotate a log file still
    /// wants their core started, and a full disk that stops a log from moving
    /// is not a reason to refuse to run. What did happen is logged.
    fn rotate_logs(&self, supervisor: &Supervisor) {
        let settings = &self.settings.logs;
        for log in [self.paths.core_log(), self.paths.app_log()] {
            match supervisor.rotate_log(&log, settings.max_size_bytes, settings.keep) {
                Ok(Some(path)) => tracing::info!(file = %path.display(), "log rotated"),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(log = %log.display(), error = %error, "could not rotate the log");
                }
            }
            match supervisor.prune_logs(&log, settings.keep_days) {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "old rotated logs deleted"),
                Err(error) => {
                    tracing::warn!(log = %log.display(), error = %error, "could not prune the logs");
                }
            }
        }
    }

    /// Stop the core.
    ///
    /// # Errors
    /// Propagates a signal failure.
    pub fn stop_core(&self) -> Result<bool> {
        self.supervisor().stop()
    }

    /// Stop and start the core.
    ///
    /// # Errors
    /// Propagates stop and start failures.
    pub fn restart_core(&self) -> Result<u32> {
        self.stop_core()?;
        self.start_core()
    }

    /// Generate, write, and hand the configuration to the core.
    ///
    /// # Errors
    /// [`Error::Validation`] when the result has errors and `force` is not
    /// set; otherwise propagates generation or commit failures.
    pub async fn apply(&self, force: bool, mode: ReloadMode) -> Result<ApplyReport> {
        let outcome = self.generate()?;
        let pipeline = self.pipeline();
        pipeline.commit(&outcome, force)?;
        let reload = self.reload(mode).await?;
        // A reload rebuilds every group, so the choice a user made this morning
        // is gone by the afternoon. It is replayed here rather than at each
        // caller, because "apply" is the operation that discards it.
        //
        // Not after a rollback, though: the core is running the document that
        // was there *before* this apply, and the choices just read belong to
        // the profile that failed to apply. Replaying them would point a
        // restored configuration at members it may not have.
        let selections_restored = if let ReloadOutcome::RolledBack { .. } = &reload {
            0
        } else {
            {
                // Before the choices, and before this returns. `/version`
                // answering means the *process* is up, not that it has this
                // configuration: a reload rebuilds the groups in the
                // background, and for a moment afterwards a group the document
                // declares is not there. Reporting success in that window made
                // `apply` mean "the file was written", while the very next
                // command failed with `no group named PROXY` — so the wait is
                // for the document, not for the process.
                self.wait_for_document(&outcome.config).await;
                self.restore_selections().await.unwrap_or(0)
            }
        };
        Ok(ApplyReport {
            outcome,
            reload: Some(reload),
            written: true,
            selections_restored,
        })
    }

    /// Copy the user's own state into a timestamped directory.
    ///
    /// What is copied is what a person would have to recreate by hand if it
    /// were lost: the settings, the profile index, the profile documents and
    /// the overrides. Not the generated configuration, which is derived and is
    /// already snapshotted by the pipeline, and not the core's working
    /// directory or the logs, which are large and reproducible.
    ///
    /// # Errors
    /// [`Error::Io`] when a file cannot be read or written, and
    /// [`Error::InvalidValue`] when two backups would land in the same second.
    pub fn backup(&self) -> Result<PathBuf> {
        // Named by the second it was taken, and given a suffix when that
        // second is taken. Refusing the second one instead — which is what
        // this did first — made `restore` impossible to use directly after a
        // backup, because restore takes a safety backup first and the two
        // land in the same second whenever a person is doing it by hand.
        // *Reserved*, not merely chosen. `exists()` and then a write is a race
        // two backups in the same second both win: both saw the same free name,
        // both wrote into one directory, and each pruned around a directory the
        // other was still filling. `create_dir` fails when the name is taken,
        // and that failure is the lock.
        let destination = self.reserve_backup_path(Utc::now().timestamp())?;
        copy_state(self.paths.home(), &destination)?;
        // Pruned around the new one, never through it. Ordering by timestamp is
        // right — the newest backups are the ones worth keeping — but it is the
        // *filesystem's* timestamps, and a directory holding five entries named
        // later than now (a clock that ran fast, a restored archive, a backup
        // copied from another machine) puts the backup just taken at the end of
        // the list. Pruning then deletes it and returns a path to a directory
        // that no longer exists, which is what `restore` builds its safety copy
        // with.
        self.prune_backups_keeping(BACKUP_LIMIT, &destination)?;
        Ok(destination)
    }

    /// A backup name that is not already taken.
    /// A backup name that is not already taken, *created* so that it stays
    /// that way.
    ///
    /// `exists()` and then a write is a race two backups in the same second
    /// both win: both see the same free name, both write into one directory,
    /// and each prunes around a directory the other is still filling.
    /// `create_dir` fails when the name is taken, and that failure is the lock.
    ///
    /// # Errors
    /// [`Error::Io`] when a name cannot be created.
    fn reserve_backup_path(&self, stamp: i64) -> Result<PathBuf> {
        let dir = self.paths.backups_dir();
        // The one directory that receives the whole state, and the only one of
        // the three this program writes into that nothing checked.
        if std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(Error::invalid(
                "backup",
                format!(
                    "{} is a symbolic link; the backups would be written outside \
                     the home",
                    dir.display()
                ),
            ));
        }
        std::fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        let mut candidates = vec![dir.join(stamp.to_string())];
        for n in 2..1000 {
            candidates.push(dir.join(format!("{stamp}-{n}")));
        }
        candidates.push(dir.join(format!("{stamp}-{}", u32::MAX)));
        for candidate in &candidates {
            match std::fs::create_dir(candidate) {
                Ok(()) => return Ok(candidate.clone()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(Error::io(candidate, e)),
            }
        }
        Err(Error::invalid(
            "backup",
            format!("a thousand backups in the second {stamp}; try again in a moment"),
        ))
    }

    /// Every backup, newest first.
    ///
    /// # Errors
    /// [`Error::Io`] when the directory cannot be read.
    pub fn backups(&self) -> Result<Vec<Backup>> {
        let dir = self.paths.backups_dir();
        // A link here is not this program's directory. Listing through it
        // offers the user's own directories as backups — the name parse and
        // nothing else — and pruning then deletes them, in a tree this program
        // never created and cannot describe.
        if std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
            tracing::warn!(
                path = %dir.display(),
                "the backups directory is a symbolic link, so it is not listed"
            );
            return Ok(Vec::new());
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut found: Vec<Backup> = entries
            .flatten()
            .filter_map(|entry| {
                // `symlink_metadata`, not `is_dir`: that follows the link, so a
                // symlink pointing at somebody else's directory was listed as a
                // backup and offered for restore. A backup is a directory this
                // program wrote, and a link is not one.
                let kind = entry.file_type().ok()?;
                if !kind.is_dir() {
                    return None;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                // `1790363784`, or `1790363784-2` when that second was taken.
                //
                // The mapping from name to `(created, sequence)` has to be
                // *injective*, because the sequence exists to break ties and a
                // tie it cannot break is the bug it was added for. So a name
                // this program would not write is not a backup: `<stamp>-2-3`,
                // `<stamp>-overflow` and `<stamp>-1` each used to parse to
                // something, and two of them to the same something.
                let (stamp, sequence) = match name.split_once('-') {
                    None => (name.as_str(), 1),
                    Some((stamp, rest)) => {
                        let n = rest.parse::<u32>().ok()?;
                        // Canonical spelling, and `-1` is the bare name.
                        if n < 2 || rest != n.to_string() {
                            return None;
                        }
                        (stamp, n)
                    }
                };
                let created = stamp.parse::<i64>().ok()?;
                // Canonicalised as well, and not only the suffix: `02000-2`
                // and `2000-2` both parsed to `(2000, 2)`, so the tie was back
                // and the survivor was the filesystem's again — canonicalising
                // one half of a key and not the other.
                if stamp != created.to_string() {
                    return None;
                }
                let items = std::fs::read_dir(entry.path())
                    .map(|inner| inner.flatten().count())
                    .unwrap_or(0);
                Some(Backup {
                    path: entry.path(),
                    created,
                    sequence,
                    items,
                })
            })
            .collect();
        // Newest first, and *within a second* by the order they were taken.
        // `created` alone left ties in `read_dir` order.
        found.sort_by_key(|backup| std::cmp::Reverse((backup.created, backup.sequence)));
        Ok(found)
    }

    /// Put a backup back, keeping the state it replaces.
    ///
    /// **Additive, not destructive.** The files the backup holds are written
    /// back; a file that appeared after it was taken stays where it is. So this
    /// is "put back what was saved", not "make the home identical to the
    /// backup", and the difference matters for a directory full of profile
    /// documents: a document no entry in the restored index mentions is an
    /// orphan, and this program preserves orphans rather than deleting them —
    /// that is what the import path was fixed to do, for the same reason.
    /// Anything a restore should not keep, a person can delete.
    ///
    /// The state being replaced is copied to a fresh backup first, so restoring
    /// the wrong one is itself undoable. That is why this is a method rather
    /// than a directory copy: the moment somebody needs it is the moment they
    /// are least sure which one they want.
    ///
    /// # Errors
    /// [`Error::InvalidValue`] when the directory is not a backup of this home,
    /// and [`Error::Io`] when a file cannot be copied.
    pub fn restore(&self, from: &Path) -> Result<PathBuf> {
        // Before anything else, because `std::fs::copy(x, x)` truncates: the
        // destination is opened for writing before a byte is read, and the copy
        // reports success having written nothing. Restoring a home onto itself
        // therefore emptied the index, the settings and every document — and it
        // *succeeded*, which is the worst way to lose data.
        //
        // Compared by canonical path, so `…/`, `…/.` and a symlink to it are
        // all recognised as the same place. A directory that cannot be
        // canonicalised is not there, and is refused by the check below.
        let same = std::fs::canonicalize(from).ok();
        if same.is_some() && same == std::fs::canonicalize(self.paths.home()).ok() {
            return Err(Error::invalid(
                "backup",
                format!(
                    "{} is this home; a restore copies over its own source and would \
                     empty it",
                    from.display()
                ),
            ));
        }
        // What `backup()` produces, rather than the one file it *usually*
        // produces: a home that has never had an index copied to a backup with
        // no `profiles.yaml` in it, and this check then refused a directory
        // this program had written itself.
        if !looks_like_a_backup(from) {
            return Err(Error::invalid(
                "backup",
                format!(
                    "{} holds none of the settings, the profile index or the profile \
                     directories, so it is not a backup of this home",
                    from.display()
                ),
            ));
        }
        // Before the safety copy, and before a single byte moves: a refusal
        // that has already written half of the backup leaves the home as two
        // configurations at once, with nothing saying so.
        check_copy(from, self.paths.home())?;
        let safety = self.backup()?;
        copy_state(from, self.paths.home())?;
        Ok(safety)
    }

    /// Keep the newest `keep` backups, and `keep_this` whatever its age.
    fn prune_backups_keeping(&self, keep: usize, keep_this: &Path) -> Result<usize> {
        let mut removed = 0;
        // Only what is *older* than the backup just taken. Two backups in the
        // same second are otherwise indistinguishable from two backups days
        // apart, and each pruned around a directory the other was still
        // filling: 24 at once left 3 of 3 returned paths holding nothing.
        let newest = self
            .backups()?
            .into_iter()
            .find(|backup| backup.path == keep_this)
            .map(|backup| (backup.created, backup.sequence));
        for backup in self.backups()?.into_iter().skip(keep) {
            if backup.path == keep_this {
                continue;
            }
            // Same *second*, not merely same-or-newer sequence. Pruning frees
            // names, and a freed name is one another `backup()` in the same
            // second will reserve and return — so two threads were handed the
            // same directory, one of them writing into what the other had
            // already handed back.
            if newest.is_some_and(|(created, _)| backup.created >= created) {
                continue;
            }
            std::fs::remove_dir_all(&backup.path).map_err(|e| Error::io(&backup.path, e))?;
            removed += 1;
        }
        removed += self.remove_links()?;
        Ok(removed)
    }

    /// Remove the symlinks in the backups directory, which are never backups.
    fn remove_links(&self) -> Result<usize> {
        let mut removed = 0;
        let dir = self.paths.backups_dir();
        // Not through a link: what is inside the target is the user's, and this
        // function deletes what it finds.
        if std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Ok(0);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(0);
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                std::fs::remove_file(entry.path()).map_err(|e| Error::io(entry.path(), e))?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Keep the newest `keep` backups and delete the rest.
    ///
    /// # Errors
    /// [`Error::Io`] when a directory exists and cannot be removed.
    pub fn prune_backups(&self, keep: usize) -> Result<usize> {
        let mut removed = 0;
        for backup in self.backups()?.into_iter().skip(keep) {
            std::fs::remove_dir_all(&backup.path).map_err(|e| Error::io(&backup.path, e))?;
            removed += 1;
        }
        // A symlink in this directory is not something this program writes, and
        // leaving it means `backup restore <name>` can be pointed at a
        // directory outside the home. Anything else a person put here stays:
        // deleting a file this program did not write and cannot explain is
        // worse than leaving it.
        removed += self.remove_links()?;
        Ok(removed)
    }

    /// Record a node choice on the current profile.
    ///
    /// # Errors
    /// Whatever reading or writing the profile index returns.
    pub fn remember_selection(&self, group: &str, member: &str) -> Result<()> {
        let mut store = self.store()?;
        store.remember_selection(group, member)?;
        store.save()
    }

    /// Forget a group's choice, which is what unpinning means.
    ///
    /// # Errors
    /// Whatever reading or writing the profile index returns.
    pub fn forget_selection(&self, group: &str) -> Result<()> {
        let mut store = self.store()?;
        store.forget_selection(group)?;
        store.save()
    }

    /// Replay the current profile's remembered choices onto the core.
    ///
    /// Best effort per group: a group the subscription has renamed, or a member
    /// it has dropped, is skipped rather than failing the others — the choice
    /// was made against a document that no longer exists, and the remaining
    /// ones are still good.
    ///
    /// # Errors
    /// [`Error::ControllerUnreachable`] when there is no core to talk to.
    pub async fn restore_selections(&self) -> Result<usize> {
        let selections = self.store()?.selections();
        if selections.is_empty() {
            return Ok(0);
        }
        let client = self.client()?;
        let overall = std::time::Instant::now() + REPLAY_TOTAL;
        let mut applied = 0;
        let mut late = Vec::new();

        // Two passes, and both are load-bearing.
        //
        // A group that is not there yet is not necessarily gone: a reload
        // rebuilds every group and the core applies it in the background, so
        // the whole reason for waiting is that the window is real. But a group
        // the subscription has *removed* is also not there, and it never will
        // be — and one pass with a budget per group let it spend that budget
        // and starve every choice after it. So the ones that are present are
        // replayed first, and only what is left is waited for.
        for selection in selections {
            if std::time::Instant::now() >= overall {
                tracing::debug!("the replay budget is spent");
                break;
            }
            let deadline = group_budget(overall, REPLAY_PER_GROUP);
            match read_group(&client, &selection.name, deadline).await {
                Some(_) => {
                    if replay_one(&client, &selection.name, &selection.now, deadline).await {
                        applied += 1;
                    }
                }
                None => late.push(selection),
            }
        }

        // Everything that was not there, polled *together* rather than one
        // after another. Waiting on each in turn gives the first group that is
        // gone the whole of its own budget and then the next one the same:
        // six groups a subscription has removed cost six waits, the total runs
        // out, and the seventh choice — the one that would have worked — is
        // never reached. A round costs one read per group still pending, and a
        // group that answers 404 costs almost nothing, so the live one is
        // replayed on the round after it appears however many dead ones precede
        // it.
        let mut pending = late;
        while !pending.is_empty() && std::time::Instant::now() < overall {
            let mut still = Vec::new();
            for selection in pending {
                let deadline = group_budget(overall, REPLAY_PER_GROUP);
                if read_group(&client, &selection.name, deadline)
                    .await
                    .is_none()
                {
                    still.push(selection);
                    continue;
                }
                if replay_one(&client, &selection.name, &selection.now, deadline).await {
                    applied += 1;
                } else {
                    tracing::debug!(
                        group = %selection.name,
                        member = %selection.now,
                        "the choice did not take; the configuration may have replaced the group"
                    );
                }
            }
            if still.is_empty() {
                break;
            }
            if std::time::Instant::now() >= overall {
                tracing::debug!(
                    groups = still.len(),
                    "the replay budget is spent before these groups came back"
                );
                break;
            }
            pending = still;
            tokio::time::sleep(REPLAY_STEP).await;
        }

        Ok(applied)
    }
    /// Hand the already-written runtime configuration to the core.
    ///
    /// Implements the decision tree described in the module docs. Never
    /// touches the filesystem beyond reading the snapshots it restores.
    ///
    /// # Errors
    /// [`Error::ControllerUnreachable`] when the core is not running and
    /// cannot be started, or when a restart does not come up in time;
    /// [`Error::CoreUnavailable`] when no core binary can be found. A failed
    /// reload whose cause cannot be undone reports *that* cause, never a
    /// failure of the rollback bookkeeping.
    pub async fn reload(&self, mode: ReloadMode) -> Result<ReloadOutcome> {
        match mode {
            ReloadMode::Restart => return self.restart_with_rollback().await,
            ReloadMode::HotReload => {
                self.hot_reload().await?;
                return Ok(ReloadOutcome::HotReloaded);
            }
            ReloadMode::Auto => {}
        }

        match self.hot_reload().await {
            Ok(()) => Ok(ReloadOutcome::HotReloaded),
            Err(reason) => {
                let reason = reason.short();
                match self.restart_with_rollback().await {
                    Ok(ReloadOutcome::Restarted { pid }) => Ok(ReloadOutcome::Restarted { pid }),
                    Ok(other) => Ok(other),
                    Err(e) => {
                        if !self.settings.core.rollback_on_failure || !e.is_rollbackable() {
                            return Err(e);
                        }
                        // The core would not come up with the new document, so
                        // put back what was working. A first apply has nothing
                        // to put back: report the restart failure — the only
                        // real information there is — rather than replacing it
                        // with a complaint about a missing snapshot.
                        if self.pipeline().snapshots()?.is_empty() {
                            return Err(e);
                        }
                        let snapshot = self.pipeline().rollback()?;
                        self.stop_core()?;
                        self.start_core()?;
                        Ok(ReloadOutcome::RolledBack { reason, snapshot })
                    }
                }
            }
        }
    }

    /// Ask the core to re-read the generated document, without restarting.
    ///
    /// # Errors
    /// [`Error::ControllerUnreachable`] when the core is not running.
    pub async fn hot_reload(&self) -> Result<()> {
        if !self.core_status().is_running() {
            return Err(Error::ControllerUnreachable {
                endpoint: self
                    .endpoint()?
                    .map_or_else(|| "unknown".to_owned(), |e| e.describe()),
                source: "the core process is not running".into(),
            });
        }
        let client = self.client()?;
        if self.settings.update.close_connections_on_apply {
            // Best effort: a core that refuses this is still reloadable.
            let _ = client.close_all_connections().await;
        }
        client
            .reload_configs(Some(&self.paths.runtime_config()), None, true)
            .await
    }

    /// Restart the core, waiting for the API to come back.
    async fn restart_with_rollback(&self) -> Result<ReloadOutcome> {
        let pid = self.restart_core()?;
        self.wait_until_ready().await?;
        Ok(ReloadOutcome::Restarted { pid })
    }

    /// Wait until the core answers for the groups this document declares.
    ///
    /// Best effort and bounded, in the shape the replay needed three attempts
    /// to find: **no call is outside the deadline**, and **no group waits on
    /// its own**. Both halves matter and both were missing here first.
    ///
    /// A sequential wait gives the first declared group that never appears the
    /// whole budget, so the groups after it are never asked about — the same
    /// starvation the replay had. And a deadline checked *between* calls is a
    /// deadline this function cannot enforce: `client.group` carries the
    /// client's own timeout, which is at least five seconds from the settings,
    /// so a core that answers `/version` and hangs `/group` held an `apply`
    /// for twice the budget it was supposed to have.
    async fn wait_for_document(&self, config: &Config) {
        let Ok(client) = self.client() else {
            return;
        };
        let groups = config.proxy_groups();
        let mut pending: Vec<&str> = groups.iter().map(|group| group.name.as_str()).collect();
        if pending.is_empty() {
            return;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !pending.is_empty() && std::time::Instant::now() < deadline {
            let mut still = Vec::new();
            for name in pending {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    still.push(name);
                    continue;
                }
                match tokio::time::timeout(left, client.group(name)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(_)) => still.push(name),
                    // The deadline itself: nothing more will be waited for.
                    Err(_) => {
                        tracing::debug!(group = name, "the core never served this group");
                        return;
                    }
                }
            }
            if still.is_empty() {
                return;
            }
            if std::time::Instant::now() >= deadline {
                tracing::debug!(
                    groups = still.len(),
                    "the configuration is live except for these groups"
                );
                return;
            }
            pending = still;
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// Wait for the controller to answer after a restart.
    ///
    /// The core binds its listener a moment after the process starts, so a
    /// request immediately after `start` legitimately fails.
    ///
    /// # Errors
    /// [`Error::ControllerUnreachable`] when it never answers within the
    /// deadline.
    pub async fn wait_until_ready(&self) -> Result<()> {
        let client = self.client()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut last = String::from("no attempt made");
        while std::time::Instant::now() < deadline {
            // Bounded, not merely checked between calls. `client.version`
            // carries the client's own timeout — `ui.refresh_ms * 5`, so at
            // least five seconds — and a core that accepts the connection and
            // answers nothing made a ten-second deadline take thirty. This is
            // the fifth place the same shape appeared.
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match tokio::time::timeout(left, client.version()).await {
                Ok(Ok(v)) => {
                    tracing::info!(version = %v.trimmed(), "core is up");
                    return Ok(());
                }
                Ok(Err(e)) => last = e.short(),
                Err(_) => break,
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        Err(Error::ControllerUnreachable {
            endpoint: self
                .endpoint()?
                .map_or_else(|| "unknown".to_owned(), |e| e.describe()),
            source: format!("the core did not become ready: {last}").into(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::profile::item::PrfItem;
    use tempfile::TempDir;

    const BASE: &str = r#"
mixed-port: 7890
external-controller: 127.0.0.1:9090
mode: rule
proxies:
  - { name: "JP 01", type: vless, server: 1.2.3.4, port: 443, uuid: u }
proxy-groups:
  - { name: PROXY, type: select, proxies: ["JP 01", DIRECT] }
rules:
  - MATCH,PROXY
"#;

    struct Fixture {
        _dir: TempDir,
        service: Service,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().unwrap();
        let service = Service::open(AppPaths::new(dir.path())).unwrap();
        Fixture { _dir: dir, service }
    }

    impl Fixture {
        fn seed(&self) -> String {
            let mut store = self.service.store().unwrap();
            let uid = store.add(PrfItem::local("L1", "base"));
            let item = store.get(&uid).unwrap().clone();
            store.write_document(&item, BASE).unwrap();
            store.set_current(&uid).unwrap();
            store.save().unwrap();
            uid
        }
    }

    #[test]
    fn opening_creates_the_home_and_loads_default_settings() {
        let f = fixture();
        assert!(f.service.paths().profiles_dir().is_dir());
        assert_eq!(*f.service.settings(), Settings::default());
        assert!(f.service.store().unwrap().items().is_empty());
    }

    #[test]
    fn a_malformed_settings_file_stops_the_service_from_opening() {
        let dir = TempDir::new().unwrap();
        let paths = AppPaths::new(dir.path());
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.settings_file(), "core: [oops\n").unwrap();
        let err = Service::open(paths).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
    }

    #[test]
    fn settings_survive_a_round_trip_through_the_service() {
        let mut f = fixture();
        let mut s = f.service.settings().clone();
        s.ui.refresh_ms = 250;
        f.service.set_settings(s.clone());
        f.service.save_settings().unwrap();
        let reopened = Service::open(f.service.paths().clone()).unwrap();
        assert_eq!(*reopened.settings(), s);
    }

    #[test]
    fn an_invalid_setting_is_refused_before_it_reaches_the_disk() {
        let mut f = fixture();
        let mut s = f.service.settings().clone();
        s.test.concurrency = 0;
        f.service.set_settings(s);
        assert!(f.service.save_settings().is_err());
        assert!(!f.service.paths().settings_file().exists());
    }

    #[test]
    fn there_is_no_endpoint_before_anything_is_generated() {
        let f = fixture();
        assert!(f.service.endpoint().unwrap().is_none());
        let err = f.service.client().unwrap_err();
        assert!(
            matches!(
                err,
                Error::MissingField {
                    field: "external-controller",
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn the_generated_runtime_config_is_the_authority_on_the_endpoint() {
        let f = fixture();
        f.seed();
        let outcome = f.service.generate().unwrap();
        f.service.pipeline().commit(&outcome, false).unwrap();
        let endpoint = f.service.endpoint().unwrap().unwrap();
        assert_eq!(endpoint, Endpoint::tcp("127.0.0.1:9090", None));
        assert!(endpoint.is_loopback());
    }

    #[test]
    fn the_endpoint_falls_back_to_the_selected_profile() {
        let f = fixture();
        f.seed();
        // Nothing generated yet, but the profile declares a controller.
        let endpoint = f.service.endpoint().unwrap().unwrap();
        assert_eq!(endpoint, Endpoint::tcp("127.0.0.1:9090", None));
    }

    #[test]
    fn a_client_can_be_built_once_an_endpoint_is_known() {
        let f = fixture();
        f.seed();
        let client = f.service.client().unwrap();
        assert!(client.endpoint().is_loopback());
    }

    #[test]
    fn core_status_is_reported_without_a_binary() {
        let f = fixture();
        assert_eq!(f.service.core_status(), CoreStatus::Stopped);
        assert!(
            f.service.core_binary().is_none(),
            "nothing is on PATH in the test home"
        );
    }

    #[test]
    fn starting_without_a_binary_explains_how_to_fix_it() {
        let f = fixture();
        f.seed();
        f.service
            .pipeline()
            .commit(&f.service.generate().unwrap(), false)
            .unwrap();
        let err = f.service.start_core().unwrap_err();
        match err {
            Error::CoreUnavailable { reason } => {
                assert!(reason.contains("mihomo"), "{reason}");
                assert!(
                    reason.contains("CVT_CORE"),
                    "the message names the override: {reason}"
                );
            }
            other => panic!("expected CoreUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn starting_without_a_generated_configuration_is_refused() {
        let f = fixture();
        let err = f.service.start_core().unwrap_err();
        // The missing binary is detected first, which is the more useful
        // message when both are missing.
        assert!(matches!(err, Error::CoreUnavailable { .. }), "{err:?}");
    }

    #[test]
    fn an_invalid_generated_configuration_is_refused_before_the_core_sees_it() {
        let f = fixture();
        let mut store = f.service.store().unwrap();
        let uid = store.add(PrfItem::local("L1", "broken"));
        let item = store.get(&uid).unwrap().clone();
        store
            .write_document(
                &item,
                "mixed-port: 7890\nexternal-controller: 127.0.0.1:9090\nrules:\n  - DOMAIN,a.test,GHOST\n",
            )
            .unwrap();
        store.set_current(&uid).unwrap();
        store.save().unwrap();

        // Force the document onto disk despite the validation errors.
        f.service
            .pipeline()
            .commit(&f.service.generate().unwrap(), true)
            .unwrap();

        // A fake core binary, so the binary check passes and validation runs.
        let fake = f.service.paths().core_dir().join("mihomo");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        let err = f.service.start_core().unwrap_err();
        assert!(matches!(err, Error::Validation { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_hot_reload_against_a_dead_core_fails_rather_than_pretending() {
        let f = fixture();
        f.seed();
        f.service
            .pipeline()
            .commit(&f.service.generate().unwrap(), false)
            .unwrap();
        let err = f.service.hot_reload().await.unwrap_err();
        assert!(
            matches!(err, Error::ControllerUnreachable { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("not running"), "{err}");
    }

    #[tokio::test]
    async fn waiting_for_a_core_that_never_answers_times_out_with_a_reason() {
        let f = fixture();
        f.seed();
        f.service
            .pipeline()
            .commit(&f.service.generate().unwrap(), false)
            .unwrap();
        // Nothing listens on 127.0.0.1:9090 in the test environment.
        let err = f.service.wait_until_ready().await.unwrap_err();
        assert!(
            matches!(err, Error::ControllerUnreachable { .. }),
            "{err:?}"
        );
        assert!(err.to_string().contains("did not become ready"), "{err}");
    }

    #[test]
    fn apply_without_a_current_profile_reports_the_missing_base() {
        let f = fixture();
        let err = f.service.generate().unwrap_err();
        assert!(err.to_string().contains("current"), "{err}");
    }

    #[tokio::test]
    async fn replaying_nothing_asks_the_core_for_nothing() {
        // The ordinary answer for a profile nobody has chosen a node in: no
        // core is needed, and none is dialled — the check is that this returns
        // without one rather than that it returns a particular number.
        let f = fixture();
        f.seed();
        assert_eq!(f.service.restore_selections().await.unwrap(), 0);
    }

    #[test]
    fn the_backup_just_taken_is_never_pruned_by_its_own_prune() {
        let f = fixture();
        f.seed();
        // Five entries named *later* than now: a clock that ran fast, an archive
        // restored from another machine, a hand-made directory. The new backup
        // sorts last, so pruning by age alone deletes the one thing the call
        // exists to produce and returns a path to nothing.
        let future = Utc::now().timestamp() + 1000;
        for offset in 0..BACKUP_LIMIT {
            let dir = f
                .service
                .paths()
                .backups_dir()
                .join((future + i64::try_from(offset).unwrap_or(i64::MAX)).to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("profiles.yaml"), "current: L1\nitems: []\n").unwrap();
        }

        let created = f.service.backup().unwrap();
        assert!(
            created.join("profiles.yaml").is_file(),
            "`backup()` returned {} and pruned it away",
            created.display()
        );
        assert!(
            f.service
                .backups()
                .unwrap()
                .iter()
                .any(|b| b.path == created),
            "and it is not even listed"
        );
    }

    #[test]
    fn a_restore_keeps_the_state_it_replaces_even_when_the_backups_directory_is_full() {
        let f = fixture();
        f.seed();
        f.service.save_settings().unwrap();
        let future = Utc::now().timestamp() + 1000;
        // A real backup to restore from, and five entries named *later* than it
        // — so the safety copy `restore` takes, stamped now, sorts last of all.
        let source = f.service.paths().backups_dir().join(future.to_string());
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("profiles.yaml"), "current: L1\nitems: []\n").unwrap();
        std::fs::write(source.join("cvt.yaml"), "ui:\n  refresh_ms: 250\n").unwrap();
        for offset in 1..=BACKUP_LIMIT {
            let dir = f
                .service
                .paths()
                .backups_dir()
                .join((future + i64::try_from(offset).unwrap_or(i64::MAX)).to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("profiles.yaml"), "current: L1\nitems: []\n").unwrap();
        }

        let safety = f.service.restore(&source).unwrap();
        assert!(
            safety.join("profiles.yaml").is_file() && safety.join("cvt.yaml").is_file(),
            "the restore said the state it replaced was kept at {}, and it is not there",
            safety.display()
        );
    }

    /// A core that answers nothing must not hold an apply for twice its budget.
    ///
    /// `wait_for_document` checked its deadline *between* `client.group()`
    /// calls, and that call carries the client's own timeout — at least five
    /// seconds from the settings. So a core that answered `/version` and hung
    /// `/group` held `apply` for about ten: a deadline the function could not
    /// enforce, which is the same mistake the selection replay was fixed for
    /// three times over.
    #[tokio::test]
    async fn waiting_for_a_document_is_bounded_by_its_own_deadline() {
        // A listener that accepts and never answers: the shape a reloading core
        // has while it is rebuilding its groups.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            // Held open and never answered, which is the shape being tested.
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
            drop(held);
        });

        let mut f = fixture();
        f.seed();
        let mut settings = f.service.settings().clone();
        settings.ui.refresh_ms = 1000;
        settings.core.external_controller = Some(format!("127.0.0.1:{port}"));
        f.service.set_settings(settings);
        let config = Config::from_yaml(
            "proxy-groups:\n  - {name: a, type: select, proxies: [DIRECT]}\n  \
             - {name: b, type: select, proxies: [DIRECT]}\nrules: ['MATCH,a']\n",
        )
        .unwrap();

        let started = std::time::Instant::now();
        f.service.wait_for_document(&config).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(7),
            "the wait took {elapsed:?} for a five-second budget; a call is \
             outside the deadline"
        );
    }

    #[test]
    fn a_backup_holds_what_a_person_would_have_to_recreate() {
        let f = fixture();
        f.seed();
        // A home that has never saved its settings has no settings file to
        // copy, which is correct — the defaults are implied — so the realistic
        // case is the one worth asserting on.
        f.service.save_settings().unwrap();
        let outcome = f.service.generate().unwrap();
        f.service.pipeline().commit(&outcome, false).unwrap();

        let path = f.service.backup().unwrap();
        for name in ["cvt.yaml", "profiles.yaml", "profiles", "overrides"] {
            assert!(
                path.join(name).exists(),
                "{name} is missing from the backup"
            );
        }
        // Derived, large and reproducible: not the point of a backup, and the
        // reason the two directories are documented as different things.
        assert!(!path.join("runtime").exists());
        assert!(!path.join("logs").exists());
        assert_eq!(f.service.backups().unwrap().len(), 1);
    }

    #[test]
    fn restoring_puts_the_state_back_and_keeps_what_it_replaced() {
        let f = fixture();
        let uid = f.seed();
        let taken = f.service.backup().unwrap();
        std::fs::write(
            f.service.paths().profiles_dir().join("tail.yaml"),
            "later\n",
        )
        .unwrap();

        let safety = f.service.restore(&taken).unwrap();
        // The state being replaced is kept, so restoring the wrong backup is
        // itself undoable: `tail.yaml` is in the safety copy…
        assert!(safety.join("profiles").join("tail.yaml").exists());
        // …and it is still where it was, because a restore is additive. It is
        // now a document no index entry mentions, which this program preserves
        // on purpose — see the method's documentation.
        assert!(f.service.paths().profiles_dir().join("tail.yaml").exists());
        assert!(f.service.store().unwrap().get(&uid).is_some());
    }

    #[test]
    fn a_directory_that_is_not_a_backup_is_refused() {
        let f = fixture();
        f.seed();
        let elsewhere = tempfile::TempDir::new().unwrap();
        let error = f.service.restore(elsewhere.path()).unwrap_err();
        assert!(
            error.to_string().contains("not a backup"),
            "a restore has to say why it refused: {error}"
        );
    }

    #[test]
    fn only_the_newest_backups_are_kept() {
        let f = fixture();
        f.seed();
        // Five is the limit, and the directories are named by the second they
        // were taken, so this has to stand on one per second to make five
        // distinct ones.
        for _ in 0..BACKUP_LIMIT + 2 {
            // Never fails: a second backup in the same second is given a
            // suffixed name rather than refused.
            f.service.backup().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        let kept = f.service.backups().unwrap();
        assert_eq!(kept.len(), BACKUP_LIMIT, "the oldest two should be gone");
        assert!(
            !kept.iter().any(|backup| backup.name().is_empty()),
            "a backup with no name could not be restored by name"
        );
    }

    #[test]
    fn reload_mode_labels_are_stable() {
        assert_eq!(ReloadMode::Auto.label(), "auto");
        assert_eq!(ReloadMode::HotReload.label(), "hot reload");
        assert_eq!(ReloadMode::Restart.label(), "restart");
    }

    #[test]
    fn reload_outcomes_describe_themselves() {
        assert!(ReloadOutcome::HotReloaded.succeeded());
        assert!(ReloadOutcome::Restarted { pid: 7 }.succeeded());
        assert_eq!(
            ReloadOutcome::Restarted { pid: 7 }.summary(),
            "restarted (pid 7)"
        );

        let rolled = ReloadOutcome::RolledBack {
            reason: "bad yaml".to_owned(),
            snapshot: PathBuf::from("/tmp/s.yaml"),
        };
        assert!(!rolled.succeeded(), "a rollback means the request failed");
        assert!(
            rolled.summary().contains("restored"),
            "{}",
            rolled.summary()
        );
    }

    #[test]
    fn the_supervisor_and_pipeline_share_the_service_home() {
        let f = fixture();
        assert_eq!(
            f.service.supervisor().log_file(),
            f.service.paths().core_log()
        );
        assert_eq!(
            f.service.pipeline().output_path(),
            f.service.paths().runtime_config()
        );
    }

    /// Point the service at a binary that exists but always fails, i.e. a core
    /// that refuses every document. This is the case rollback exists for, and
    /// it needs no real core on the test machine.
    fn refuse_everything(f: &mut Fixture) {
        let mut s = f.service.settings().clone();
        s.core.binary = Some(PathBuf::from("/bin/false"));
        s.core.rollback_on_failure = true;
        f.service.set_settings(s);
    }

    /// Finding F19: `reload` replaced the reason a restart failed with
    /// `InvalidValue { field: "rollback" }` — "there are no snapshots to
    /// restore" — because `Pipeline::rollback` was called through `?` on a
    /// service whose first document had never been committed. `config generate
    /// --apply` therefore blamed rollback bookkeeping for a missing core and
    /// for a document the core rejected.
    #[tokio::test]
    async fn a_failed_reload_reports_the_real_cause_not_the_rollback_bookkeeping() {
        let mut f = fixture();
        f.seed();
        // A first apply: the document is written, but nothing was there
        // before it, so the pipeline has no snapshot to restore.
        let outcome = f.service.generate().unwrap();
        f.service.pipeline().commit(&outcome, false).unwrap();
        assert!(f.service.pipeline().snapshots().unwrap().is_empty());
        refuse_everything(&mut f);

        let err = f.service.reload(ReloadMode::Auto).await.unwrap_err();
        assert!(
            !matches!(
                &err,
                Error::InvalidValue {
                    field: "rollback",
                    ..
                }
            ),
            "bookkeeping must never displace the cause: {err:?}"
        );
        assert!(
            !err.to_string().contains("snapshot"),
            "nothing to roll back to is not a diagnosis: {err}"
        );
        assert!(
            matches!(err, Error::ProcessFailed { .. }),
            "the core's own refusal is the real cause: {err:?}"
        );
    }

    /// The complement: when a previous document *is* on disk, a rejected one is
    /// still undone, so the test above cannot pass by disabling rollback.
    #[tokio::test]
    async fn a_rejected_document_is_still_rolled_back_when_there_is_one_to_restore() {
        let mut f = fixture();
        let uid = f.seed();
        let first = f.service.generate().unwrap();
        f.service.pipeline().commit(&first, false).unwrap();

        {
            let store = f.service.store().unwrap();
            let item = store.get(&uid).unwrap().clone();
            store
                .write_document(&item, &BASE.replace("7890", "7891"))
                .unwrap();
            store.save().unwrap();
        }
        let second = f.service.generate().unwrap();
        f.service.pipeline().commit(&second, false).unwrap();
        assert_ne!(first.yaml, second.yaml, "the two documents must differ");

        refuse_everything(&mut f);
        // The restart cannot succeed with a core that refuses everything, but
        // the document that was working has to be back on disk.
        let _ = f.service.reload(ReloadMode::Auto).await;

        let restored = std::fs::read_to_string(f.service.paths().runtime_config()).unwrap();
        assert_eq!(
            restored, first.yaml,
            "the working document must be restored"
        );
    }
}
