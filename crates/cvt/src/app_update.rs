//! Official application releases, persisted reminder choices and atomic installation.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use cvt_core::AppPaths;
use cvt_tui::app::UpdateRelease;
use futures_util::StreamExt as _;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const REPO: &str = "retort-hubbub/clash-verge-tui";
const MAX_ARCHIVE: usize = 64 * 1024 * 1024;
const MAX_BINARY: u64 = 128 * 1024 * 1024;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Preferences {
    skipped: Option<String>,
    remind_after: i64,
    last_check: i64,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    body: Option<String>,
    html_url: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

fn preferences(paths: &AppPaths) -> Result<Preferences> {
    let path = paths.home().join("app-update.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(serde_json::from_str(&text).context("reading update preferences")?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Preferences::default()),
        Err(e) => Err(e.into()),
    }
}

fn save(paths: &AppPaths, prefs: &Preferences) -> Result<()> {
    paths.write_atomic(
        &paths.home().join("app-update.json"),
        &serde_json::to_string_pretty(prefs)?,
    )?;
    Ok(())
}

pub fn dismiss(paths: &AppPaths, tag: &str, skip: bool) -> Result<()> {
    let mut prefs = preferences(paths)?;
    if skip {
        prefs.skipped = Some(tag.to_owned());
    } else {
        prefs.remind_after = chrono::Utc::now().timestamp() + 3600;
    }
    save(paths, &prefs)
}

fn client(proxy: Option<String>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(concat!("clash-verge-tui/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180));
    if let Some(proxy) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    Ok(builder.build()?)
}

async fn release(client: &reqwest::Client, suffix: &str) -> Result<Release> {
    let response = client
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/{suffix}"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?;
    let bytes = bounded_body(response, 2 * 1024 * 1024).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn version(tag: &str) -> Result<Version> {
    Ok(Version::parse(tag.strip_prefix('v').unwrap_or(tag))?)
}

fn asset_name(tag: &str) -> String {
    format!("clash-verge-tui-{tag}-{}.tar.gz", env!("CVT_BUILD_TARGET"))
}

fn asset(release: &Release) -> Result<&Asset> {
    let expected = asset_name(&release.tag_name);
    release
        .assets
        .iter()
        .find(|item| item.name == expected)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "release {} has no binary for {}; please try again after release builds finish",
                release.tag_name,
                env!("CVT_BUILD_TARGET")
            )
        })
}

/// Stable releases only; manual checks can revisit skipped or postponed versions.
pub async fn check(
    paths: &AppPaths,
    proxy: Option<String>,
    manual: bool,
    installed: Option<String>,
) -> Result<Option<UpdateRelease>> {
    let mut prefs = preferences(paths)?;
    let now = chrono::Utc::now().timestamp();
    if !manual && (now < prefs.remind_after || now.saturating_sub(prefs.last_check) < 3600) {
        return Ok(None);
    }
    let client = client(proxy)?;
    let latest = release(&client, "latest").await?;
    prefs = preferences(paths)?;
    prefs.last_check = now;
    save(paths, &prefs)?;
    let current = version(installed.as_deref().unwrap_or(env!("CARGO_PKG_VERSION")))?;
    let newest = version(&latest.tag_name)?;
    if latest.draft
        || latest.prerelease
        || !newest.pre.is_empty()
        || newest <= current
        || (!manual
            && (now < prefs.remind_after || prefs.skipped.as_deref() == Some(&latest.tag_name)))
    {
        return Ok(None);
    }
    asset(&latest)?;
    let summary: String = latest
        .body
        .unwrap_or_default()
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .take(900)
        .collect();
    Ok(Some(UpdateRelease {
        tag: latest.tag_name,
        summary,
        url: latest.html_url,
    }))
}

async fn bounded_body(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > limit {
            bail!("release download exceeds its size limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn extract(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut binary = None;
    let mut total = 0u64;
    for (index, entry) in archive.entries()?.enumerate() {
        let mut entry = entry?;
        total = total.saturating_add(entry.size());
        if index > 128 || total > MAX_BINARY * 2 {
            bail!("release archive exceeds its unpacking limit");
        }
        if entry
            .path()?
            .file_name()
            .is_some_and(|name| name == "clash-verge-tui")
        {
            if !entry.header().entry_type().is_file()
                || entry.size() == 0
                || entry.size() > MAX_BINARY
                || binary.is_some()
            {
                bail!("release archive has an invalid application executable");
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            binary = Some(bytes);
        }
    }
    binary.ok_or_else(|| anyhow::anyhow!("release archive has no application executable"))
}

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Validate the pinned release, extract only its executable, and replace by rename.
/// The old inode stays available to the current process and in a sibling backup.
pub async fn install(tag: &str, proxy: Option<String>) -> Result<PathBuf> {
    if !cfg!(target_os = "linux") {
        bail!("automatic application updates currently support Linux release binaries only");
    }
    let wanted = version(tag)?;
    if !wanted.pre.is_empty() || wanted <= version(env!("CARGO_PKG_VERSION"))? {
        bail!("refusing an application downgrade or prerelease");
    }
    let destination = std::fs::canonicalize(std::env::current_exe()?)?;
    let metadata = std::fs::metadata(&destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o6000 != 0 {
            bail!("automatic updates cannot replace a setuid/setgid executable");
        }
    }
    let parent = destination
        .parent()
        .context("application executable has no parent directory")?;
    let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = format!(".clash-verge-tui-update-{}-{seq}", std::process::id());
    let temp = parent.join(&name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o700);
    }
    let mut file = options.open(&temp)
        .context("the executable directory is not writable; update this installation with its package manager or install it in a user-writable directory")?;
    let staged = Staging(temp);
    let client = client(proxy)?;
    let latest = release(&client, &format!("tags/{tag}")).await?;
    if version(&latest.tag_name)? != wanted || latest.draft || latest.prerelease {
        bail!("release changed or is not stable");
    }
    let asset = asset(&latest)?;
    let digest = asset
        .digest
        .as_deref()
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        .context("release asset has no valid SHA-256 digest")?;
    let bytes = bounded_body(
        client
            .get(&asset.browser_download_url)
            .send()
            .await?
            .error_for_status()?,
        MAX_ARCHIVE,
    )
    .await?;
    if !format!("{:x}", Sha256::digest(&bytes)).eq_ignore_ascii_case(digest) {
        bail!("application SHA-256 mismatch; original executable was not changed");
    }
    let binary = tokio::task::spawn_blocking(move || extract(&bytes)).await??;
    file.write_all(&binary)?;
    file.sync_all()?;
    drop(file);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            &staged.0,
            std::fs::Permissions::from_mode(metadata.permissions().mode() & 0o755),
        )?;
    }
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(&staged.0)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("new executable version check timed out")??;
    let printed = String::from_utf8_lossy(&output.stdout);
    if !output.status.success()
        || printed
            .split_whitespace()
            .last()
            .and_then(|s| version(s).ok())
            != Some(wanted)
    {
        bail!("new executable failed its version check; original executable was not changed");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let current = std::fs::metadata(&destination)?;
        if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
            bail!("application executable changed during download; restart before updating again");
        }
    }
    let backup = parent.join(format!("{name}.previous"));
    std::fs::hard_link(&destination, &backup).context("preserving the previous executable")?;
    if let Err(error) = std::fs::rename(&staged.0, &destination) {
        let _ = std::fs::remove_file(&backup);
        return Err(error).context("replacing the application executable");
    }
    if let Err(error) = std::fs::File::open(parent).and_then(|directory| directory.sync_all()) {
        tracing::warn!(%error, "application replaced but directory sync failed");
    }
    Ok(backup)
}
