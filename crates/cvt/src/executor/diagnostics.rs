//! Diagnostics adapter for TUI effects.

use cvt_tui::app::{IpInfo, SpeedMode};
use futures_util::StreamExt as _;
use std::time::{Duration, Instant};

/// Query the public exit from the same local proxy used by unlock checks.
pub(super) async fn fetch_exit_ip(address: &str) -> Result<IpInfo, String> {
    let proxy =
        reqwest::Proxy::all(format!("http://{address}")).map_err(|error| error.to_string())?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(8))
        .user_agent(concat!("clash-verge-tui/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())?;
    let mut errors = Vec::new();
    for (name, url) in [
        ("ipapi.co", "https://ipapi.co/json/"),
        ("ip.sb", "https://api.ip.sb/geoip"),
    ] {
        let result = async {
            let response = client
                .get(url)
                .send()
                .await
                .map_err(|e| e.to_string())?
                .error_for_status()
                .map_err(|e| e.to_string())?
                .json::<serde_json::Value>()
                .await
                .map_err(|e| e.to_string())?;
            parse_exit_ip(&response)
        }
        .await;
        match result {
            Ok(info) => return Ok(info),
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    Err(errors.join("; "))
}

pub(super) fn parse_exit_ip(response: &serde_json::Value) -> Result<IpInfo, String> {
    let ip = response["ip"]
        .as_str()
        .filter(|ip| ip.parse::<std::net::IpAddr>().is_ok())
        .ok_or_else(|| "IP lookup did not return an address".to_owned())?;
    Ok(IpInfo {
        ip: ip.to_owned(),
        country: response["country_name"]
            .as_str()
            .or_else(|| response["country"].as_str())
            .unwrap_or("-")
            .to_owned(),
        organization: response["org"]
            .as_str()
            .or_else(|| response["organization"].as_str())
            .unwrap_or("-")
            .to_owned(),
    })
}

/// The chosen backend always uses the active Mihomo route.
pub(super) async fn measure_route_speed(
    address: &str,
    mode: SpeedMode,
    home: &std::path::Path,
) -> Result<String, String> {
    if let Some(bytes) = mode.sample_bytes() {
        return download_speed(Some(address), bytes)
            .await
            .map(|rate| format!("{rate:.1} Mbit/s ({} MB sample)", bytes / 1_000_000));
    }
    let binary = crate::speedtest::find_binary(home)
        .ok_or_else(|| "speedtest-go is not installed".to_owned())?;
    let output = tokio::time::timeout(
        Duration::from_secs(45),
        tokio::process::Command::new(binary)
            .arg("--proxy")
            .arg(format!("http://{address}"))
            .arg("--saving-mode")
            .arg("--no-upload")
            .arg("--json")
            .arg("--thread")
            .arg("2")
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let output = output
        .map_err(|_| "speedtest-go timed out".to_owned())?
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if detail.is_empty() {
            format!("speedtest-go exited with {}", output.status)
        } else {
            detail
        });
    }
    let rate = speedtest_download_mbps(&output.stdout)
        .ok_or_else(|| "speedtest-go returned no download rate".to_owned())?;
    Ok(format!("{rate:.1} Mbit/s (speedtest-go)"))
}

/// `speedtest-go` JSON stores `dl_speed` as bytes per second.
pub(super) fn speedtest_download_mbps(stdout: &[u8]) -> Option<f64> {
    let value: serde_json::Value = serde_json::from_slice(stdout).ok()?;
    let bytes_per_second = value["servers"][0]["dl_speed"].as_f64()?;
    (bytes_per_second.is_finite() && bytes_per_second > 0.0)
        .then_some(bytes_per_second * 8.0 / 1_000_000.0)
}

/// A bounded download through the deployed HTTP/mixed listener. The current
/// routing mode and selected policy determine the exit; no group is mutated.
pub(super) async fn download_speed(
    proxy_addr: Option<&str>,
    sample_bytes: usize,
) -> Result<f64, String> {
    let address =
        proxy_addr.ok_or_else(|| "no HTTP or mixed proxy listener is deployed".to_owned())?;
    let proxy =
        reqwest::Proxy::all(format!("http://{address}")).map_err(|error| error.to_string())?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(match sample_bytes {
            0..=4_000_000 => 20,
            4_000_001..=20_000_000 => 60,
            _ => 180,
        }))
        .build()
        .map_err(|error| error.to_string())?;
    let began = Instant::now();
    let response = client
        .get(format!(
            "https://speed.cloudflare.com/__down?bytes={sample_bytes}"
        ))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let mut stream = response.bytes_stream();
    let mut bytes = 0_usize;
    while let Some(chunk) = stream.next().await {
        bytes += chunk.map_err(|error| error.to_string())?.len();
        if bytes >= sample_bytes {
            break;
        }
    }
    if bytes < sample_bytes / 2 {
        return Err(format!("speed test returned only {bytes} bytes"));
    }
    Ok((bytes as f64 * 8.0) / began.elapsed().as_secs_f64() / 1_000_000.0)
}
