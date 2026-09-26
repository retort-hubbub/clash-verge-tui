//! `profiles` — the subscription index and the documents behind it.
//!
//! The index is the user's only record of their subscriptions, so the
//! commands here are conservative: nothing is removed implicitly, a rename
//! never moves a file, and a failed download leaves the profile in place to be
//! refreshed again.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use cvt_core::error::Error;
use cvt_core::profile::item::PrfItem;
use cvt_core::profile::source::{UpdateOutcome, is_due};
use cvt_core::profile::store::{ImportReport, ProfileStore};
use serde::Serialize;

use crate::cli::{AddArgs, ChainArgs, ProfilesCommand, UpdateArgs};
use crate::context::Ctx;
use crate::exit::Exit;
use crate::output::{self, Fields, Output, Report, Table};

/// Run one `profiles` subcommand.
///
/// # Errors
/// Whatever the operation failed with.
pub async fn run(ctx: &Ctx, command: &ProfilesCommand) -> Result<()> {
    match command {
        ProfilesCommand::List => list(ctx),
        ProfilesCommand::Add(args) => add(ctx, args).await,
        ProfilesCommand::Remove { uid } => remove(ctx, uid),
        ProfilesCommand::Rename { uid, name } => rename(ctx, uid, name),
        ProfilesCommand::EditUrl { uid, url, no_fetch } => edit_url(ctx, uid, url, *no_fetch).await,
        ProfilesCommand::Switch { uid } => switch(ctx, uid),
        ProfilesCommand::Update(args) => update(ctx, args).await,
        ProfilesCommand::Import { dir } => import(ctx, dir),
        ProfilesCommand::Chain(args) => chain(ctx, args),
        ProfilesCommand::Show { uid } => show(ctx, uid),
    }
}

/// One row of `profiles list`.
#[derive(Debug, Serialize)]
pub struct ProfileRow {
    /// Uid.
    pub uid: String,
    /// Display name.
    pub name: String,
    /// Profile type.
    pub kind: &'static str,
    /// Whether this is the base of the generated configuration.
    pub current: bool,
    /// Position in the explicit chain, when it is in one.
    pub chain_position: Option<usize>,
    /// Subscription URL, for remote profiles.
    pub url: Option<String>,
    /// Last successful refresh, as a unix timestamp.
    pub updated: Option<i64>,
    /// Whether a refresh is due now.
    pub due: bool,
    /// Path of the document.
    pub document: String,
    /// Whether the document is there.
    pub document_exists: bool,
}

/// Everything `profiles list` prints.
#[derive(Debug, Serialize)]
pub struct ProfileListReport {
    /// Uid of the current profile.
    pub current: Option<String>,
    /// Whether the chain is explicit or implied by profile type.
    pub explicit_chain: bool,
    /// Every profile, in index order.
    pub profiles: Vec<ProfileRow>,
}

impl Report for ProfileListReport {
    fn schema(&self) -> &'static str {
        "cvt.profiles.list.v1"
    }

    fn render(&self, _out: Output) -> String {
        if self.profiles.is_empty() {
            return "no profiles; add one with `clash-verge-tui profiles add <url>`".to_owned();
        }
        let mut table = Table::new(["", "uid", "name", "type", "chain", "updated", "url"]);
        for profile in &self.profiles {
            table.push([
                if profile.current { "*" } else { "" }.to_owned(),
                profile.uid.clone(),
                profile.name.clone(),
                profile.kind.to_owned(),
                profile
                    .chain_position
                    .map_or_else(|| "-".to_owned(), |p| p.to_string()),
                output::timestamp(profile.updated),
                profile.url.clone().unwrap_or_else(|| "-".to_owned()),
            ]);
        }
        let mut text = table.render();
        let _ = write!(
            text,
            "\n\n{} profile(s); `*` is the base of the generated configuration",
            self.profiles.len()
        );
        if self.explicit_chain {
            text.push_str("; `chain` is the explicit application order");
        }
        text
    }
}

fn list(ctx: &Ctx) -> Result<()> {
    let store = ctx.store()?;
    let now = now_unix();
    let profile_rows = store
        .items()
        .iter()
        .map(|item| ProfileRow {
            uid: item.uid.clone(),
            name: item.label().to_owned(),
            kind: item.kind.as_str(),
            current: store.current_uid() == Some(item.uid.as_str()),
            chain_position: store
                .index()
                .chain
                .iter()
                .position(|uid| uid == &item.uid)
                .map(|p| p + 1),
            url: item.url.clone(),
            updated: item.updated,
            due: item.is_remote() && is_due(item, now),
            document: cvt_core::profile::store::document_path(ctx.paths(), item)
                .display()
                .to_string(),
            document_exists: cvt_core::profile::store::document_path(ctx.paths(), item).is_file(),
        })
        .collect();
    let report = ProfileListReport {
        current: store.current_uid().map(str::to_owned),
        explicit_chain: !store.index().chain.is_empty(),
        profiles: profile_rows,
    };
    ctx.out().emit(&report)
}

/// What happened to one profile.
#[derive(Debug, Serialize)]
pub struct ProfileChangeReport {
    /// `added`, `removed`, `renamed` or `switched`.
    pub action: &'static str,
    /// Uid the action was about.
    pub uid: String,
    /// Display name after the action.
    pub name: String,
    /// Anything else worth saying.
    pub detail: String,
}

impl Report for ProfileChangeReport {
    fn schema(&self) -> &'static str {
        "cvt.profiles.changed.v1"
    }

    fn render(&self, _out: Output) -> String {
        format!("{} `{}` ({})", self.action, self.name, self.uid)
    }
}

async fn add(ctx: &Ctx, args: &AddArgs) -> Result<()> {
    let url = args.url.trim().to_owned();
    if url.is_empty() {
        return Err(Error::invalid("url", "the subscription URL is empty").into());
    }
    // Before the profile exists, and with the fetcher's own rule: an address
    // this program cannot fetch is one the index should never have held, and
    // finding out afterwards leaves a profile pointing at nothing.
    cvt_core::profile::source::check_fetchable(&url)?;
    let name = args.name.clone().unwrap_or_else(|| default_name(&url));
    let uid = ctx.edit_store(|store| Ok(store.add(PrfItem::remote("", name, url.clone()))))?;
    ctx.out()
        .verbose(1, format!("added {uid}; downloading the subscription"));

    let result = fetch_one(ctx, &uid).await;
    // The profile exists either way. A failed download is reported as its own
    // result rather than by undoing the add: retrying is one command, and
    // re-adding would lose the name the user chose.
    let ok = result.ok;
    // A panel that names the subscription knows better than the host name this
    // fell back to, and it only says so when the document arrives. Adopted only
    // when the user did not name it: a name somebody chose is theirs.
    if ok
        && args.name.is_none()
        && let Some(suggested) = result.suggested_name.clone()
    {
        ctx.edit_store(|store| store.rename(&uid, &suggested))?;
        ctx.out().verbose(
            1,
            format!("named `{uid}` {suggested:?}, as the panel suggested"),
        );
    }
    ctx.out().emit(&AddReport {
        action: "added",
        uid: uid.clone(),
        url,
        ok,
        result,
    })?;
    if !ok {
        return Err(Exit::failure(format!(
            "profile {uid} was added but could not be downloaded; retry with `clash-verge-tui profiles update {uid}`"
        ))
        .into());
    }
    Ok(())
}

/// The result of `profiles add`.
#[derive(Debug, Serialize)]
pub struct AddReport {
    /// Always `added`.
    pub action: &'static str,
    /// Uid assigned to the new profile.
    pub uid: String,
    /// URL the profile was created for.
    pub url: String,
    /// Whether the first download succeeded.
    pub ok: bool,
    /// What the download did.
    pub result: UpdateRow,
}

impl Report for AddReport {
    fn schema(&self) -> &'static str {
        "cvt.profiles.added.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut fields = Fields::new();
        // The action, not the word "added": `profiles edit-url` reports through
        // this shape too, and printing "added" for a URL that was changed is a
        // report of something that did not happen.
        fields.push(self.action, format!("{} ({})", self.uid, self.url));
        fields.push("download", self.result.describe());
        fields.push_opt(
            "hint",
            self.result.error.clone().map(|error| {
                format!(
                    "{error}; retry with `clash-verge-tui profiles update {}`",
                    self.uid
                )
            }),
        );
        fields.render()
    }
}

fn remove(ctx: &Ctx, uid: &str) -> Result<()> {
    let removed = ctx.edit_store(|store| store.remove(uid))?;
    let Some(item) = removed else {
        return Err(Error::ProfileNotFound {
            uid: uid.to_owned(),
        }
        .into());
    };
    ctx.out().emit(&ProfileChangeReport {
        action: "removed",
        uid: item.uid.clone(),
        name: item.label().to_owned(),
        detail: format!(
            "the document {} was deleted with it",
            cvt_core::profile::store::document_path(ctx.paths(), &item).display()
        ),
    })
}

fn rename(ctx: &Ctx, uid: &str, name: &str) -> Result<()> {
    ctx.edit_store(|store| store.rename(uid, name))?;
    ctx.out().emit(&ProfileChangeReport {
        action: "renamed",
        uid: uid.to_owned(),
        name: name.to_owned(),
        detail: "the uid and the document are unchanged".to_owned(),
    })
}

/// Change a subscription's URL, and by default fetch from the new one.
///
/// Fetching by default because a URL that has been changed and not fetched is
/// the *old* provider's document with the new provider's address beside it in
/// the index — an inconsistency that looks like a working profile until the
/// next update quietly replaces it. `--no-fetch` exists for the case where the
/// new provider is not reachable yet, and says what it left behind.
async fn edit_url(ctx: &Ctx, uid: &str, url: &str, no_fetch: bool) -> Result<()> {
    let url = url.trim().to_owned();
    if url.is_empty() {
        return Err(Error::invalid("url", "the subscription URL is empty").into());
    }
    let previous = ctx.edit_store(|store| {
        let previous = store.get(uid).and_then(|item| item.url.clone());
        store.set_url(uid, &url)?;
        Ok(previous)
    })?;

    if no_fetch {
        ctx.out().warn(format!(
            "`{uid}` now points at {url}, but its document is still the one fetched from {}; \
             run `clash-verge-tui profiles update {uid}` before the next generate",
            previous.as_deref().unwrap_or("its previous URL")
        ));
        return ctx.out().emit(&UrlChangeReport {
            uid: uid.to_owned(),
            url,
            previous,
            fetched: false,
            download: None,
        });
    }

    let result = fetch_one(ctx, uid).await;
    let ok = result.ok;
    let mut reverted = false;
    if !ok {
        // Put back. The index is written before the fetch because the fetcher
        // reads the URL from it, and leaving the new one behind on a failure
        // means every refusal leaves the profile pointing at something that
        // cannot download — the state `--no-fetch` exists to *warn* about, and
        // this path made it the default.
        if let Some(previous) = previous.as_deref() {
            ctx.edit_store(|store| store.set_url(uid, previous))?;
            reverted = true;
        }
    }
    // What the profile points at *now*, which after a rollback is not what was
    // asked for. Reporting the requested address made the failure say "points
    // at the new URL" about a profile pointing at the old one, and advised a
    // retry that would have retried the address it had just reverted.
    let now = if reverted {
        previous.clone().unwrap_or_default()
    } else {
        url.clone()
    };
    ctx.out().emit(&UrlChangeReport {
        uid: uid.to_owned(),
        url: now.clone(),
        // What it pointed at before this command, which after a rollback is
        // the address it still points at. The pair reading the same is how a
        // caller sees that the change was undone: a `previous` that differs
        // from `url` describes a change this command made.
        previous: if reverted {
            Some(now.clone())
        } else {
            previous.clone()
        },
        fetched: true,
        download: Some(result),
    })?;
    if !ok {
        return Err(Exit::failure(if reverted {
            format!(
                "`{uid}` could not be downloaded from the new address, so it still points at \
                 `{now}`; fix the address and try again with \
                 `clash-verge-tui profiles edit-url {uid} <url>`"
            )
        } else {
            format!(
                "`{uid}` points at the new URL but could not be downloaded from it; retry with \
                 `clash-verge-tui profiles update {uid}`"
            )
        })
        .into());
    }
    Ok(())
}

/// The result of `profiles edit-url`.
///
/// One shape for one command. The first version reported through `AddReport`
/// when it fetched and `ProfileChangeReport` when it did not, so a caller
/// parsing `--json` had to handle `cvt.profiles.added.v1` and
/// `cvt.profiles.changed.v1` for the same operation — and the first of those
/// says "added" about something that was not added.
#[derive(Debug, Serialize)]
pub struct UrlChangeReport {
    /// The profile whose URL changed.
    pub uid: String,
    /// The URL it now points at.
    pub url: String,
    /// What it pointed at before, when it had a URL at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    /// Whether the new URL was downloaded from.
    pub fetched: bool,
    /// The download, when there was one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download: Option<UpdateRow>,
}

impl Report for UrlChangeReport {
    fn schema(&self) -> &'static str {
        "cvt.profiles.url.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut fields = Fields::new();
        fields.push("url", format!("{} -> {}", self.uid, self.url));
        fields.push_opt("was", self.previous.clone());
        if let Some(download) = &self.download {
            fields.push("download", download.describe());
        } else {
            fields.push("document", "still the previous provider's".to_owned());
        }
        fields.render()
    }
}

fn switch(ctx: &Ctx, uid: &str) -> Result<()> {
    let item = ctx.edit_store(|store| {
        store.set_current(uid)?;
        Ok(store.get(uid).map(|i| i.label().to_owned()))
    })?;
    let name = item.unwrap_or_else(|| uid.to_owned());
    if let Ok(store) = ctx.store()
        && let Some(item) = store.get(uid)
        && store.read_document(item).is_err()
    {
        ctx.out().warn(format!(
            "the document for `{name}` is not readable yet; run `clash-verge-tui profiles update {uid}` \
             or `clash-verge-tui config generate` will fail"
        ));
    }
    ctx.out().emit(&ProfileChangeReport {
        action: "switched",
        uid: uid.to_owned(),
        name,
        detail: "run `clash-verge-tui config generate --apply` to deploy it".to_owned(),
    })
}

/// One profile's refresh result.
#[derive(Debug, Serialize)]
pub struct UpdateRow {
    /// Profile uid, or `-` when it never got as far as one.
    pub uid: String,
    /// A name the panel suggested, when it sent one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_name: Option<String>,
    /// Whether the refresh produced a document.
    pub ok: bool,
    /// Bytes received.
    pub bytes: usize,
    /// Route that worked.
    pub source: Option<String>,
    /// Whether the provider sent exactly what was already stored.
    pub unchanged: bool,
    /// Tiers that failed before one worked.
    pub failed_tiers: usize,
    /// One line describing the outcome.
    pub summary: String,
    /// Why it failed, as one line.
    pub error: Option<String>,
}

impl UpdateRow {
    fn from_outcome(outcome: &UpdateOutcome) -> Self {
        Self {
            uid: outcome.uid.clone(),
            suggested_name: outcome.suggested_name.clone(),
            ok: true,
            bytes: outcome.bytes,
            source: Some(outcome.source.to_string()),
            unchanged: outcome.unchanged,
            failed_tiers: outcome.failed_attempts(),
            summary: outcome.summary(),
            error: None,
        }
    }

    fn from_error(uid: &str, error: &Error) -> Self {
        Self {
            uid: uid.to_owned(),
            suggested_name: None,
            ok: false,
            bytes: 0,
            source: None,
            unchanged: false,
            failed_tiers: 0,
            summary: format!("{uid}: failed"),
            error: Some(error.short()),
        }
    }

    /// The one-line description used by callers that show a single result.
    fn describe(&self) -> String {
        match (&self.error, self.unchanged) {
            (Some(error), _) => format!("failed: {error}"),
            (None, true) => format!("{} bytes, already up to date", self.bytes),
            (None, false) => format!(
                "{} bytes via {}",
                self.bytes,
                self.source.as_deref().unwrap_or("unknown")
            ),
        }
    }
}

/// The result of `profiles update`.
#[derive(Debug, Serialize)]
pub struct UpdateReport {
    /// One row per profile that was refreshed.
    pub results: Vec<UpdateRow>,
    /// Profiles that succeeded.
    pub succeeded: usize,
    /// Profiles that failed.
    pub failed: usize,
}

impl Report for UpdateReport {
    fn schema(&self) -> &'static str {
        "cvt.profiles.update.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut table = Table::new(["uid", "result", "route", "bytes"]);
        for row in &self.results {
            table.push([
                row.uid.clone(),
                if row.ok {
                    if row.unchanged {
                        "unchanged".to_owned()
                    } else {
                        "updated".to_owned()
                    }
                } else {
                    format!(
                        "failed: {}",
                        output::ellipsis(row.error.as_deref().unwrap_or("unknown"), 60)
                    )
                },
                row.source.clone().unwrap_or_else(|| "-".to_owned()),
                row.bytes.to_string(),
            ]);
        }
        format!(
            "{}\n\n{} updated, {} failed",
            table.render(),
            self.succeeded,
            self.failed
        )
    }
}

async fn update(ctx: &Ctx, args: &UpdateArgs) -> Result<()> {
    let fetcher = ctx.fetcher()?;
    let mut store = ctx.store()?;

    let mut results: Vec<UpdateRow> = Vec::new();
    if args.all_due {
        let outcome = fetcher.update_all_due(&mut store).await;
        if outcome.is_empty() {
            ctx.out().note("no remote profile is due for a refresh");
        }
        for (uid, result) in outcome {
            results.push(match result {
                Ok(outcome) => UpdateRow::from_outcome(&outcome),
                Err(error) => UpdateRow::from_error(&uid, &error),
            });
        }
    } else {
        let uid = match args.uid.clone() {
            Some(uid) => uid,
            None => store
                .current_uid()
                .map(str::to_owned)
                .ok_or_else(|| Error::MissingField {
                    uid: "-".to_owned(),
                    field: "current",
                })?,
        };
        results.push(fetch_one_with(&fetcher, &mut store, &uid).await);
    }

    let failed = results.iter().filter(|row| !row.ok).count();
    let report = UpdateReport {
        succeeded: results.len() - failed,
        failed,
        results,
    };
    ctx.out().emit(&report)?;
    if failed > 0 {
        return Err(Exit::failure(format!("{failed} profile(s) could not be refreshed")).into());
    }
    Ok(())
}

async fn fetch_one(ctx: &Ctx, uid: &str) -> UpdateRow {
    let fetcher = match ctx.fetcher() {
        Ok(fetcher) => fetcher,
        Err(error) => {
            return UpdateRow::from_error(
                uid,
                &Error::Http {
                    url: "(unknown)".to_owned(),
                    source: error.to_string().into(),
                },
            );
        }
    };
    match ctx.store() {
        Ok(mut store) => fetch_one_with(&fetcher, &mut store, uid).await,
        Err(error) => {
            UpdateRow::from_error(uid, &Error::invalid("profile index", error.to_string()))
        }
    }
}

async fn fetch_one_with(
    fetcher: &cvt_core::profile::source::SubscriptionFetcher,
    store: &mut ProfileStore,
    uid: &str,
) -> UpdateRow {
    match fetcher.update(store, uid).await {
        Ok(outcome) => UpdateRow::from_outcome(&outcome),
        Err(error) => UpdateRow::from_error(uid, &error),
    }
}

/// What `profiles import` did.
#[derive(Debug, Serialize)]
pub struct ImportInfo {
    /// Where the profiles came from.
    pub source: String,
    /// Profiles added.
    pub imported: usize,
    /// Profiles whose uid collided and had to change.
    pub renamed: usize,
    /// Documents copied.
    pub documents_copied: usize,
    /// The library's one-line summary.
    pub summary: String,
}

impl From<(&Path, ImportReport)> for ImportInfo {
    fn from((source, report): (&Path, ImportReport)) -> Self {
        Self {
            source: source.display().to_string(),
            imported: report.imported,
            renamed: report.renamed,
            documents_copied: report.documents_copied,
            summary: report.summary(),
        }
    }
}

impl Report for ImportInfo {
    fn schema(&self) -> &'static str {
        "cvt.profiles.import.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut fields = Fields::new();
        fields.push("source", &self.source);
        fields.push("imported", self.imported.to_string());
        fields.push("renamed", self.renamed.to_string());
        fields.push("documents copied", self.documents_copied.to_string());
        fields.render()
    }
}

fn import(ctx: &Ctx, dir: &Path) -> Result<()> {
    let report = ctx.edit_store(|store| store.import_from(dir))?;
    ctx.out().emit(&ImportInfo::from((dir, report)))
}

/// One entry of the resolved chain.
#[derive(Debug, Serialize)]
pub struct ChainEntry {
    /// Position in the chain, starting at 1.
    pub position: usize,
    /// Uid.
    pub uid: String,
    /// Display name.
    pub name: String,
    /// Profile type.
    pub kind: &'static str,
    /// Whether the profile is skipped when the configuration is generated.
    pub skipped: bool,
    /// Why it is skipped, when it is.
    pub note: Option<String>,
}

/// The result of `profiles chain`.
#[derive(Debug, Serialize)]
pub struct ChainReport {
    /// Uid of the base profile.
    pub current: Option<String>,
    /// Whether the order is explicit, rather than implied by profile type.
    pub explicit: bool,
    /// The chain, in application order.
    pub entries: Vec<ChainEntry>,
}

impl Report for ChainReport {
    fn schema(&self) -> &'static str {
        "cvt.profiles.chain.v1"
    }

    fn render(&self, _out: Output) -> String {
        if self.entries.is_empty() {
            return "the chain is empty".to_owned();
        }
        let mut table = Table::new(["#", "uid", "name", "type", "note"]);
        for entry in &self.entries {
            table.push([
                entry.position.to_string(),
                entry.uid.clone(),
                entry.name.clone(),
                entry.kind.to_owned(),
                entry.note.clone().unwrap_or_else(|| "-".to_owned()),
            ]);
        }
        format!(
            "{}\n\n{} {} profile(s), base first",
            table.render(),
            self.entries.len(),
            if self.explicit { "explicit" } else { "implied" }
        )
    }
}

fn chain(ctx: &Ctx, args: &ChainArgs) -> Result<()> {
    if !args.uids.is_empty() || args.clear {
        ctx.edit_store(|store| store.set_chain(&args.uids))?;
    }
    let store = ctx.store()?;
    let resolve = store.resolve_chain();
    let (entries, failure) = match resolve {
        Ok(chain) => (
            chain
                .iter()
                .enumerate()
                .map(|(index, item)| ChainEntry {
                    position: index + 1,
                    uid: item.uid.clone(),
                    name: item.label().to_owned(),
                    kind: item.kind.as_str(),
                    skipped: item.unsupported_reason().is_some(),
                    note: item.unsupported_reason(),
                })
                .collect(),
            None,
        ),
        Err(error) => (Vec::new(), Some(error)),
    };
    let report = ChainReport {
        current: store.current_uid().map(str::to_owned),
        explicit: !store.index().chain.is_empty(),
        entries,
    };
    ctx.out().emit(&report)?;
    if let Some(error) = failure {
        return Err(error.into());
    }
    Ok(())
}

/// The result of `profiles show`.
#[derive(Debug, Serialize)]
pub struct ProfileShowReport {
    /// Uid.
    pub uid: String,
    /// Display name.
    pub name: String,
    /// Profile type.
    pub kind: &'static str,
    /// Document filename inside the profiles directory.
    pub file: String,
    /// Path of the document.
    pub path: String,
    /// The document itself.
    pub document: String,
}

impl Report for ProfileShowReport {
    fn schema(&self) -> &'static str {
        "cvt.profiles.show.v1"
    }

    fn render(&self, _out: Output) -> String {
        // The document verbatim, so `cvt profiles show uid > out.yaml` is a
        // valid copy of what the pipeline will read.
        if self.document.ends_with('\n') {
            self.document.clone()
        } else {
            format!("{}\n", self.document)
        }
    }
}

fn show(ctx: &Ctx, uid: &str) -> Result<()> {
    let store = ctx.store()?;
    let item = store.get(uid).ok_or_else(|| Error::ProfileNotFound {
        uid: uid.to_owned(),
    })?;
    let document = store.read_document(item)?;
    ctx.out().emit(&ProfileShowReport {
        uid: item.uid.clone(),
        name: item.label().to_owned(),
        kind: item.kind.as_str(),
        file: item.file_name(),
        path: cvt_core::profile::store::document_path(ctx.paths(), item)
            .display()
            .to_string(),
        document,
    })
}

/// A display name for a subscription that has none: its host.
#[must_use]
pub fn default_name(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    let host = authority.split('@').next_back().unwrap_or(authority);
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        url.to_owned()
    } else {
        host.to_owned()
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use cvt_core::AppPaths;
    use cvt_core::profile::item::ProfileType;
    use tempfile::TempDir;

    fn ctx(dir: &TempDir) -> Ctx {
        Ctx::open(AppPaths::new(dir.path()), Output::new(false, 0, false)).unwrap()
    }

    fn seed(ctx: &Ctx) -> String {
        ctx.edit_store(|store| {
            let uid = store.add(PrfItem::remote("R1", "Airport", "https://example.com/sub"));
            store.add(PrfItem::patch("m1", "Merge", ProfileType::Merge));
            let item = store.get(&uid).unwrap().clone();
            store.write_document(&item, "mode: rule\n")?;
            store.set_current(&uid)?;
            Ok(uid)
        })
        .unwrap()
    }

    #[test]
    fn a_subscription_name_falls_back_to_its_host() {
        assert_eq!(
            default_name("https://example.com/sub?token=x"),
            "example.com"
        );
        assert_eq!(
            default_name("https://user:pw@panel.example.com:8443/s"),
            "panel.example.com"
        );
        assert_eq!(default_name("http://127.0.0.1:8080"), "127.0.0.1");
        assert_eq!(default_name("garbage"), "garbage");
    }

    #[test]
    fn listing_reports_the_current_profile_and_the_chain_position() {
        let dir = TempDir::new().unwrap();
        let ctx = ctx(&dir);
        seed(&ctx);
        ctx.edit_store(|store| store.set_chain(&["m1".to_owned()]))
            .unwrap();

        let store = ctx.store().unwrap();
        assert_eq!(store.current_uid(), Some("R1"));
        let report = ProfileListReport {
            current: store.current_uid().map(str::to_owned),
            explicit_chain: !store.index().chain.is_empty(),
            profiles: store
                .items()
                .iter()
                .map(|item| ProfileRow {
                    uid: item.uid.clone(),
                    name: item.label().to_owned(),
                    kind: item.kind.as_str(),
                    current: store.current_uid() == Some(item.uid.as_str()),
                    chain_position: store
                        .index()
                        .chain
                        .iter()
                        .position(|uid| uid == &item.uid)
                        .map(|p| p + 1),
                    url: item.url.clone(),
                    updated: item.updated,
                    due: false,
                    document: String::new(),
                    document_exists: true,
                })
                .collect(),
        };
        let value = output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.profiles.list.v1"));
        assert_eq!(value["current"], serde_json::json!("R1"));
        assert_eq!(value["profiles"][0]["current"], serde_json::json!(true));
        assert_eq!(value["profiles"][1]["chain_position"], serde_json::json!(1));
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("Airport"), "{text}");
        assert!(text.contains('*'), "the current profile is marked: {text}");
    }

    #[test]
    fn an_empty_index_says_how_to_start() {
        let report = ProfileListReport {
            current: None,
            explicit_chain: false,
            profiles: Vec::new(),
        };
        assert!(
            report
                .render(Output::new(false, 0, false))
                .contains("profiles add")
        );
    }

    #[test]
    fn showing_a_profile_prints_the_document_verbatim() {
        let report = ProfileShowReport {
            uid: "R1".into(),
            name: "A".into(),
            kind: "remote",
            file: "R1.yaml".into(),
            path: "/x/R1.yaml".into(),
            document: "mode: rule".into(),
        };
        assert_eq!(report.render(Output::new(false, 0, false)), "mode: rule\n");
        let value = output::to_value(&report).unwrap();
        assert_eq!(value["schema"], serde_json::json!("cvt.profiles.show.v1"));
        assert_eq!(value["document"], serde_json::json!("mode: rule"));
    }

    #[test]
    fn the_chain_lists_the_skipped_profiles_rather_than_hiding_them() {
        let entry = ChainEntry {
            position: 2,
            uid: "s1".into(),
            name: "tweaks".into(),
            kind: "script",
            skipped: true,
            note: Some("JavaScript is not executed".into()),
        };
        let report = ChainReport {
            current: Some("R1".into()),
            explicit: false,
            entries: vec![entry],
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("JavaScript"), "{text}");
        let value = output::to_value(&report).unwrap();
        assert_eq!(value["entries"][0]["skipped"], serde_json::json!(true));
    }

    #[test]
    fn an_update_row_describes_a_failure_without_losing_the_reason() {
        let error = Error::Http {
            url: "https://x".into(),
            source: "connection refused".into(),
        };
        let row = UpdateRow::from_error("R1", &error);
        assert!(!row.ok);
        assert!(row.describe().starts_with("failed: "));
        let report = UpdateReport {
            results: vec![row],
            succeeded: 0,
            failed: 1,
        };
        let text = report.render(Output::new(false, 0, false));
        assert!(text.contains("0 updated, 1 failed"), "{text}");
    }
}
