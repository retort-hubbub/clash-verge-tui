//! One explicit authentication grants capabilities and narrow resolved access.
use anyhow::{Context as _, Result, bail};
use cvt_core::mihomo::resolver;
use std::path::Path;
use std::process::Command;

/// Privileged entry point. It is dispatched before opening any application home.
/// No shell code, arbitrary command, or subscription-selected interface is run.
pub fn internal(args: &[std::ffi::OsString]) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        bail!("Linux authorization is unavailable on this platform");
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if !root() || args.len() != 3 {
            bail!("invalid privileged core authorization invocation");
        }
        let binary = std::fs::canonicalize(&args[0]).context("locating the approved core")?;
        let caps = args[1].to_str().context("capability encoding")?;
        if caps.is_empty()
            || caps
                .split(',')
                .any(|cap| !["cap_net_admin", "cap_net_raw", "cap_net_bind_service"].contains(&cap))
        {
            bail!("invalid capability request");
        }
        let uid: u32 = args[2].to_str().context("uid encoding")?.parse()?;
        if let Some(caller) =
            std::env::var_os("PKEXEC_UID").or_else(|| std::env::var_os("SUDO_UID"))
            && caller.to_str().and_then(|s| s.parse::<u32>().ok()) != Some(uid)
        {
            bail!("authorization user mismatch");
        }
        if !binary.is_file() {
            bail!("the approved core is not a regular file");
        }
        // Preserve existing capability clauses exactly, including flag groups.
        let getcap = tool("getcap")?;
        let existing = resolver::command(
            Command::new(getcap).arg(&binary),
            std::time::Duration::from_secs(5),
        )?;
        let existing = existing
            .trim()
            .strip_prefix(binary.to_string_lossy().as_ref())
            .unwrap_or("")
            .trim();
        let grant = format!("{existing} {caps}+ep");
        resolver::command(
            Command::new(tool("setcap")?).arg(grant.trim()).arg(&binary),
            std::time::Duration::from_secs(5),
        )?;
        if caps.split(',').any(|cap| cap == "cap_net_admin") && resolver::available() {
            let passwd = resolver::command(
                Command::new("/usr/bin/getent").args(["passwd", &uid.to_string()]),
                std::time::Duration::from_secs(5),
            )?;
            let user = passwd
                .split(':')
                .next()
                .filter(|s| !s.is_empty())
                .context("resolving the approved user")?;
            let dir = Path::new("/etc/polkit-1/rules.d");
            let metadata = std::fs::symlink_metadata(dir).context("locating polkit rules")?;
            if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
                bail!("polkit rule directory is not private root-owned state");
            }
            let dest = dir.join(format!("49-clash-verge-tui-{uid}-resolver.rules"));
            let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
            use std::io::Write as _;
            temporary.write_all(resolver::policy(user).as_bytes())?;
            temporary.as_file().sync_all()?;
            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o644))?;
            temporary.persist(&dest).map_err(|error| error.error)?;
        }
        Ok(())
    }
}

fn tool(name: &str) -> Result<String> {
    [format!("/usr/bin/{name}"), format!("/usr/sbin/{name}")]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .context("libcap tools are unavailable")
}

/// The kernel effective UID, independent of login environment variables.
pub fn root() -> bool {
    uid() == Some(0)
}

/// The kernel effective UID used to scope resolver authorization.
pub fn uid() -> Option<u32> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find(|line| line.starts_with("Uid:"))?
        .split_whitespace()
        .nth(2)?
        .parse()
        .ok()
}
