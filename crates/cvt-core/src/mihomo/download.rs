//! Downloading and updating the mihomo core.
//!
//! Downloads official prebuilt releases from MetaCubeX/mihomo on GitHub,
//! extracts the executable, and places it into the managed core directory
//! under the application home (`<home>/core/mihomo`).

use sha2::{Digest as _, Sha256};
use std::io::{Read, Write};
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mihomo::supervisor::Supervisor;
use crate::paths::AppPaths;

const GITHUB_REPO: &str = "MetaCubeX/mihomo";

/// Asset descriptor from GitHub's releases API.
#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
    size: u64,
}

/// Release descriptor from GitHub's releases API.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

/// The platform OS name used in mihomo release asset filenames.
#[must_use]
pub fn current_platform_os() -> Option<&'static str> {
    match std::env::consts::OS {
        "linux" => Some("linux"),
        "macos" => Some("darwin"),
        "windows" => Some("windows"),
        _ => None,
    }
}

/// The platform architecture name used in mihomo release asset filenames.
#[must_use]
pub fn current_platform_arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("amd64"),
        "aarch64" => Some("arm64"),
        "x86" => Some("386"),
        "arm" => Some("armv7"),
        _ => None,
    }
}

/// Expected executable file name on this platform.
#[must_use]
pub fn core_executable_name() -> &'static str {
    if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    }
}

/// Find the best matching release asset for the current OS and architecture.
fn select_asset<'a>(assets: &'a [GithubAsset], os: &str, arch: &str) -> Option<&'a GithubAsset> {
    let prefix = format!("mihomo-{os}-{arch}");
    let expected_ext = if os == "windows" { ".zip" } else { ".gz" };

    let mut candidates: Vec<&'a GithubAsset> = assets
        .iter()
        .filter(|a| {
            a.name.starts_with(&prefix)
                && a.name.ends_with(expected_ext)
                && !a.name.contains(".deb")
                && !a.name.contains(".rpm")
                && !a.name.contains(".pkg")
        })
        .collect();

    // Prefer standard over compatible or specific go versions
    candidates.sort_by_key(|a| {
        let mut penalty = 0;
        if a.name.contains("compatible") {
            penalty += 10;
        }
        if a.name.contains("go12") {
            penalty += 5;
        }
        (penalty, a.name.len())
    });

    candidates.first().copied()
}

/// Download and install the latest mihomo release from GitHub into `<home>/core`.
///
/// Returns the version after archive integrity and execution compatibility checks.
/// GitHub metadata is the digest trust source; this is not signature verification.
///
/// # Errors
/// [`Error::Unsupported`] on unsupported platforms, [`Error::Http`] or
/// [`Error::Io`] on download or filesystem failures, and [`Error::ProcessFailed`]
/// if the downloaded binary fails execution verification.
pub async fn install_latest_core(paths: &AppPaths) -> Result<String> {
    let proxy = crate::Service::open(paths.clone())?.proxy_addr();
    install_latest_core_with_proxy(paths, proxy.as_deref()).await
}

/// Install using the application-owned running core as the first download route.
///
/// # Errors
/// As [`install_latest_core`].
pub async fn install_latest_core_with_proxy(
    paths: &AppPaths,
    proxy: Option<&str>,
) -> Result<String> {
    tokio::time::timeout(
        Duration::from_secs(300),
        install(
            paths,
            proxy,
            std::time::Instant::now() + Duration::from_secs(300),
        ),
    )
    .await
    .map_err(|_| unavailable("core installation exceeded its five-minute deadline"))?
}

const MAX_ARCHIVE: usize = 64 * 1024 * 1024;
const MAX_BINARY: u64 = 128 * 1024 * 1024;
const MAX_RELEASE: usize = 4 * 1024 * 1024;

fn unavailable(reason: impl Into<String>) -> Error {
    Error::CoreUnavailable {
        reason: reason.into(),
    }
}

async fn install(
    paths: &AppPaths,
    proxy: Option<&str>,
    deadline: std::time::Instant,
) -> Result<String> {
    let os = current_platform_os()
        .ok_or_else(|| Error::Unsupported("unsupported core OS".to_owned()))?;
    let arch = current_platform_arch()
        .ok_or_else(|| Error::Unsupported("unsupported core architecture".to_owned()))?;
    // The extraction implementation is gzip-only; refuse before downloading a zip.
    if os == "windows" {
        return Err(Error::Unsupported(
            "automatic core installation on Windows is not supported".to_owned(),
        ));
    }
    paths.ensure_dirs()?;
    let fetcher = crate::download::ArtifactFetcher::new(proxy)?;
    let api = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let release: GithubRelease = serde_json::from_slice(&fetcher.get(&api, MAX_RELEASE).await?)
        .map_err(|e| unavailable(format!("invalid release metadata: {e}")))?;
    let asset = select_asset(&release.assets, os, arch)
        .ok_or_else(|| Error::Unsupported(format!("no core asset for {os}-{arch}")))?;
    let digest = asset
        .digest
        .as_deref()
        .and_then(|value| value.strip_prefix("sha256:"))
        .filter(|value| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| {
            unavailable("release asset has no valid SHA-256 digest; installation refused")
        })?;
    if asset.size == 0 || asset.size > MAX_ARCHIVE as u64 {
        return Err(unavailable(
            "core archive size is outside the allowed range",
        ));
    }
    let url =
        reqwest::Url::parse(&asset.browser_download_url).map_err(|e| unavailable(e.to_string()))?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url
            .path()
            .starts_with(&format!("/{GITHUB_REPO}/releases/download/"))
    {
        return Err(unavailable(
            "core download URL is outside the official release repository",
        ));
    }
    let bytes = fetcher.get(url.as_str(), MAX_ARCHIVE).await?;
    if bytes.len() as u64 != asset.size
        || format!("{:x}", Sha256::digest(&bytes)) != digest.to_ascii_lowercase()
    {
        return Err(unavailable(
            "core archive size or SHA-256 digest does not match GitHub release metadata",
        ));
    }
    // Integrity has been checked before decompression and before any execution.
    // This authenticates bytes against GitHub metadata, not an independent signature.
    let paths = paths.clone();
    let tag = release.tag_name;
    tokio::task::spawn_blocking(move || install_verified(&paths, &bytes, &tag, deadline))
        .await
        .map_err(|e| unavailable(format!("core installer failed: {e}")))?
}

fn install_verified(
    paths: &AppPaths,
    archive: &[u8],
    tag: &str,
    deadline: std::time::Instant,
) -> Result<String> {
    let directory = paths.core_dir();
    let dest = directory.join(core_executable_name());
    let mut candidate =
        tempfile::NamedTempFile::new_in(&directory).map_err(|e| Error::io(&directory, e))?;
    let mut decoder = flate2::read::GzDecoder::new(archive).take(MAX_BINARY + 1);
    let length = std::io::copy(&mut decoder, candidate.as_file_mut())
        .map_err(|e| Error::io(candidate.path(), e))?;
    if length == 0 || length > MAX_BINARY {
        return Err(unavailable(
            "decompressed core exceeds the size limit or is empty",
        ));
    }
    candidate
        .flush()
        .map_err(|e| Error::io(candidate.path(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        candidate
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::io(candidate.path(), e))?;
    }
    candidate
        .as_file()
        .sync_all()
        .map_err(|e| Error::io(candidate.path(), e))?;
    // Close the writable descriptor before exec (ETXTBSY on Linux otherwise).
    let candidate = candidate.into_temp_path();
    if std::time::Instant::now() >= deadline {
        return Err(unavailable("core installation deadline exceeded"));
    }
    let version = Supervisor::new(paths.clone()).version(&candidate)?;
    if !version.split_whitespace().any(|token| token == tag) {
        return Err(unavailable(format!(
            "downloaded core does not report the expected release {tag}"
        )));
    }
    // All download, integrity, decompression and execution failures leave the old
    // executable untouched. Keep an independent backup for later rollback.
    if let Ok(meta) = std::fs::symlink_metadata(&dest) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(unavailable(
                "managed core destination is not a regular file",
            ));
        }
        let backup = directory.join(format!("{}.previous", core_executable_name()));
        let mut saved =
            tempfile::NamedTempFile::new_in(&directory).map_err(|e| Error::io(&directory, e))?;
        std::io::copy(
            &mut std::fs::File::open(&dest).map_err(|e| Error::io(&dest, e))?,
            saved.as_file_mut(),
        )
        .map_err(|e| Error::io(&backup, e))?;
        saved
            .as_file()
            .set_permissions(meta.permissions())
            .map_err(|e| Error::io(&backup, e))?;
        saved
            .as_file()
            .sync_all()
            .map_err(|e| Error::io(&backup, e))?;
        saved
            .persist(&backup)
            .map_err(|e| Error::io(&backup, e.error))?;
    }
    if std::time::Instant::now() >= deadline {
        return Err(unavailable("core installation deadline exceeded"));
    }
    candidate
        .persist(&dest)
        .map_err(|e| Error::io(&dest, e.error))?;
    Ok(version)
}
