//! Explicit Linux authorization for a user-managed TUN core.
//!
//! The grant is attached to the selected binary, not to this application or
//! the user's whole session. Replacing the binary removes the grant.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

#[cfg(target_os = "linux")]
fn capability_tool(name: &str) -> Result<&'static str> {
    let candidates: &[&str] = match name {
        "getcap" => &["/usr/bin/getcap", "/usr/sbin/getcap"],
        "setcap" => &["/usr/bin/setcap", "/usr/sbin/setcap"],
        _ => bail!("unknown capability tool"),
    };
    candidates
        .iter()
        .copied()
        .find(|path| Path::new(path).is_file())
        .ok_or_else(|| anyhow::anyhow!("{name} is unavailable on this architecture"))
}

/// Ask the desktop policy agent to grant only the capabilities needed by TUN.
///
/// # Errors
/// Reports a missing binary, missing polkit agent, refused authentication, or
/// a capability grant that did not take effect.
pub fn authorize(binary: &Path) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = binary;
        bail!("TUN authorization is not supported on this architecture");
    }
    #[cfg(target_os = "linux")]
    {
        let binary = std::fs::canonicalize(binary).context("locating the Mihomo binary")?;
        if !binary.is_file() {
            bail!("the Mihomo binary is not a regular file");
        }
        let getcap = capability_tool("getcap")?;
        let setcap = capability_tool("setcap")?;
        let granted = || -> Result<bool> {
            let output = Command::new(getcap).arg(&binary).output()?;
            if !output.status.success() {
                bail!("could not inspect Mihomo network capabilities");
            }
            let text = String::from_utf8_lossy(&output.stdout);
            Ok(text.contains("cap_net_admin")
                && text.contains("cap_net_raw")
                && text.contains("=ep"))
        };
        if granted()? {
            return Ok(());
        }
        let status = Command::new("pkexec")
            .arg("--disable-internal-agent")
            .arg(setcap)
            .arg("cap_net_admin,cap_net_raw+ep")
            .arg(&binary)
            .status()
            .context("pkexec is unavailable; TUN needs a desktop authentication agent")?;
        if !status.success() {
            bail!("TUN authorization was refused or no desktop authentication agent is available");
        }
        if !granted()? {
            bail!("network capability grant did not take effect");
        }
        Ok(())
    }
}
