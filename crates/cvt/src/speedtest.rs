//! Optional speedtest-go backend, installed only after a TUI confirmation.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const MAX_ARCHIVE: usize = 32_000_000;
const MAX_BINARY: u64 = 64_000_000;
static INSTALL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

pub fn managed_binary(home: &Path) -> PathBuf {
    home.join("tools").join(if cfg!(windows) {
        "speedtest-go.exe"
    } else {
        "speedtest-go"
    })
}

pub fn find_binary(home: &Path) -> Option<PathBuf> {
    let managed = managed_binary(home);
    if is_executable(&managed) {
        return Some(managed);
    }
    let name = if cfg!(windows) {
        "speedtest-go.exe"
    } else {
        "speedtest-go"
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn asset_name(tag: &str) -> Result<String, String> {
    let os = match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Darwin",
        "windows" => "Windows",
        other => {
            return Err(format!(
                "speedtest-go installation is unsupported on {other}"
            ));
        }
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        "x86" => "i386",
        other => {
            return Err(format!(
                "speedtest-go installation is unsupported on {other}"
            ));
        }
    };
    let version = tag.strip_prefix('v').unwrap_or(tag);
    Ok(format!("speedtest-go_{version}_{os}_{arch}.tar.gz"))
}

/// Download a release whose GitHub asset digest matches, then install its binary.
pub async fn install(home: &Path, proxy: Option<&str>) -> Result<String, String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(300),
        install_inner(home, proxy),
    )
    .await
    .map_err(|_| "speedtest-go installation exceeded its five-minute deadline".to_owned())?
}

async fn install_inner(home: &Path, proxy: Option<&str>) -> Result<String, String> {
    let fetcher = cvt_core::download::ArtifactFetcher::new(proxy).map_err(|e| e.to_string())?;
    let metadata = fetcher
        .get(
            "https://api.github.com/repos/showwin/speedtest-go/releases/latest",
            4 * 1024 * 1024,
        )
        .await
        .map_err(|e| e.to_string())?;
    let release: Release = serde_json::from_slice(&metadata).map_err(|e| e.to_string())?;
    let name = asset_name(&release.tag_name)?;
    let asset = release
        .assets
        .iter()
        .find(|item| item.name == name)
        .ok_or_else(|| format!("release {} has no {name} asset", release.tag_name))?;
    let digest = asset
        .digest
        .as_deref()
        .and_then(|v| v.strip_prefix("sha256:"))
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| "release asset has no valid SHA-256 digest".to_owned())?;
    let archive = fetcher
        .get(&asset.browser_download_url, MAX_ARCHIVE)
        .await
        .map_err(|e| e.to_string())?;
    let actual = format!("{:x}", Sha256::digest(&archive));
    if !actual.eq_ignore_ascii_case(digest) {
        return Err("speedtest-go SHA-256 mismatch".to_owned());
    }
    let binary = extract_binary(&archive)?;
    let dest = managed_binary(home);
    let parent = dest.parent().ok_or("invalid speedtest-go destination")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let seq = INSTALL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".speedtest-go-{}-{seq}.download",
        std::process::id()
    ));
    let outcome = async {
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|e| e.to_string())?;
            file.write_all(&binary).map_err(|e| e.to_string())?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
        }
        let check = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::process::Command::new(&temp)
                .arg("--version")
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| "speedtest-go version check timed out".to_owned())?
        .map_err(|e| e.to_string())?;
        if !check.status.success() {
            return Err("downloaded speedtest-go failed its version check".to_owned());
        }
        std::fs::rename(&temp, &dest).map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    }
    .await;
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    outcome.map(|()| release.tag_name)
}

fn extract_binary(archive: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?;
        let name = if cfg!(windows) {
            "speedtest-go.exe"
        } else {
            "speedtest-go"
        };
        if entry.header().entry_type().is_file()
            && path.file_name().is_some_and(|part| part == name)
        {
            if entry.size() > MAX_BINARY {
                return Err("speedtest-go binary is unexpectedly large".to_owned());
            }
            let mut binary = Vec::new();
            entry.read_to_end(&mut binary).map_err(|e| e.to_string())?;
            return Ok(binary);
        }
    }
    Err("release archive contains no speedtest-go binary".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_asset_matches_platform_naming() {
        let name = asset_name("v1.8.3").unwrap();
        assert!(name.starts_with("speedtest-go_1.8.3_"));
        assert!(name.ends_with(".tar.gz"));
    }

    #[test]
    fn refuses_archive_without_binary() {
        assert!(extract_binary(b"not an archive").is_err());
    }
}
