//! Downloading and updating the mihomo core.
//!
//! Downloads official prebuilt releases from MetaCubeX/mihomo on GitHub,
//! extracts the executable, and places it into the managed core directory
//! under the application home (`<home>/core/mihomo`).

use std::io::Read;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mihomo::supervisor::Supervisor;
use crate::paths::AppPaths;

const GITHUB_REPO: &str = "MetaCubeX/mihomo";
const USER_AGENT: &str = "clash-verge-tui";

/// Asset descriptor from GitHub's releases API.
#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
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
/// Returns the verified version string of the newly installed binary.
///
/// # Errors
/// [`Error::Unsupported`] on unsupported platforms, [`Error::Http`] or
/// [`Error::Io`] on download or filesystem failures, and [`Error::ProcessFailed`]
/// if the downloaded binary fails execution verification.
pub async fn install_latest_core(paths: &AppPaths) -> Result<String> {
    let os = current_platform_os().ok_or_else(|| {
        Error::Unsupported(format!(
            "automatic core download is not supported on {}",
            std::env::consts::OS
        ))
    })?;
    let arch = current_platform_arch().ok_or_else(|| {
        Error::Unsupported(format!(
            "automatic core download is not supported on {}",
            std::env::consts::ARCH
        ))
    })?;

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| Error::CoreUnavailable {
            reason: format!("failed to build HTTP client: {e}"),
        })?;

    let api_url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest");
    let resp = client
        .get(&api_url)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .map_err(|e| Error::CoreUnavailable {
            reason: format!("failed to query GitHub releases: {e}"),
        })?;

    let (download_url, tag_name) = if resp.status().is_success() {
        let release: GithubRelease = resp.json().await.map_err(|e| Error::CoreUnavailable {
            reason: format!("failed to parse GitHub release response: {e}"),
        })?;
        let asset = select_asset(&release.assets, os, arch).ok_or_else(|| {
            Error::Unsupported(format!(
                "no compatible asset found for {os}-{arch} in release {}",
                release.tag_name
            ))
        })?;
        (asset.browser_download_url.clone(), release.tag_name)
    } else {
        // Fallback if GitHub API rate-limited: direct tag download URL
        return Err(Error::CoreUnavailable {
            reason: format!(
                "GitHub API returned HTTP {}: unable to locate latest release",
                resp.status()
            ),
        });
    };

    // Download the release asset
    let download_resp =
        client
            .get(&download_url)
            .send()
            .await
            .map_err(|e| Error::CoreUnavailable {
                reason: format!("failed to download core asset from {download_url}: {e}"),
            })?;

    if !download_resp.status().is_success() {
        return Err(Error::CoreUnavailable {
            reason: format!(
                "download failed with HTTP status {}",
                download_resp.status()
            ),
        });
    }

    let bytes = download_resp
        .bytes()
        .await
        .map_err(|e| Error::CoreUnavailable {
            reason: format!("failed to read download payload: {e}"),
        })?;

    // Decompress payload
    let decompressed = if Path::new(&download_url)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"))
    {
        let mut decoder = flate2::read::GzDecoder::new(&bytes[..]);
        let mut buffer = Vec::new();
        decoder
            .read_to_end(&mut buffer)
            .map_err(|e| Error::io(Path::new("mihomo.gz"), e))?;
        buffer
    } else {
        return Err(Error::Unsupported(
            "zip extraction not supported on this platform".to_owned(),
        ));
    };

    paths.ensure_dirs()?;
    let core_dir = paths.core_dir();
    let dest = core_dir.join(core_executable_name());
    let temp_dest = core_dir.join(format!("{}.download", core_executable_name()));

    std::fs::write(&temp_dest, &decompressed).map_err(|e| Error::io(&temp_dest, e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp_dest, std::fs::Permissions::from_mode(0o755));
    }

    std::fs::rename(&temp_dest, &dest).map_err(|e| Error::io(&dest, e))?;

    // Verify newly installed binary
    let supervisor = Supervisor::new(paths.clone());
    match supervisor.version(&dest) {
        Ok(ver) => {
            let ver_trimmed = ver.trim().to_owned();
            Ok(if ver_trimmed.is_empty() {
                tag_name
            } else {
                ver_trimmed
            })
        }
        Err(err) => {
            let _ = std::fs::remove_file(&dest);
            Err(err)
        }
    }
}
