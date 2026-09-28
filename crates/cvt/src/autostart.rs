//! Register the existing one-shot `cvt core start` command at user login.
//!
//! systemd and XDG desktop autostart are mutually exclusive. Registration is
//! deliberately per-user; this module never writes a system service or asks
//! for root privileges.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use cvt_core::settings::LoginAutostart;

const UNIT: &str = "clash-verge-tui.service";
const DESKTOP: &str = "clash-verge-tui.desktop";

fn config_home() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_CONFIG_HOME")
        && !value.is_empty()
    {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".config"))
        .ok_or_else(|| anyhow::anyhow!("this architecture is not supported: no user config home"))
}

fn session_supports(backend: LoginAutostart) -> bool {
    let current = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    current.split(':').any(|part| match backend {
        LoginAutostart::Kde => part.eq_ignore_ascii_case("KDE"),
        LoginAutostart::Gnome => part.eq_ignore_ascii_case("GNOME"),
        _ => false,
    })
}

fn systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .context("systemd user manager is unavailable")?;
    if !output.status.success() {
        bail!(
            "systemd user manager failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn quote_systemd(path: &Path) -> Result<String> {
    let value = path.to_str().context("startup path is not UTF-8")?;
    if value.contains(['\n', '\r']) {
        bail!("startup path contains a newline");
    }
    Ok(format!(
        "\"{}\"",
        value
            .replace('%', "%%")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    ))
}

fn quote_desktop(path: &Path) -> Result<String> {
    let value = path.to_str().context("startup path is not UTF-8")?;
    if value.contains(['\n', '\r']) {
        bail!("startup path contains a newline");
    }
    let mut escaped = String::new();
    for ch in value.chars() {
        if ch == '%' {
            escaped.push_str("%%");
            continue;
        }
        if matches!(ch, '\\' | '"' | '$' | '`') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    Ok(format!("\"{escaped}\""))
}

fn unit_contents(exe: &Path, home: &Path) -> Result<String> {
    let exe = quote_systemd(exe)?;
    let home = quote_systemd(home)?;
    Ok(format!(
        "[Unit]\nDescription=clash-verge-tui Mihomo core\n\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart={exe} --home {home} core start\nExecStop={exe} --home {home} core stop\n\n[Install]\nWantedBy=default.target\n"
    ))
}

fn desktop_contents(exe: &Path, home: &Path, backend: LoginAutostart) -> Result<String> {
    let exe = quote_desktop(exe)?;
    let home = quote_desktop(home)?;
    let desktop = match backend {
        LoginAutostart::Kde => "KDE",
        LoginAutostart::Gnome => "GNOME",
        _ => bail!("desktop autostart requires KDE or GNOME"),
    };
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=clash-verge-tui core\nExec={exe} --home {home} core start\nTerminal=false\nOnlyShowIn={desktop};\nX-GNOME-Autostart-enabled=true\n"
    ))
}

fn write_owned(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("startup entry has no parent directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        bail!("refusing symlinked startup entry {}", path.display());
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// Apply the selected user login startup method.
///
/// # Errors
/// Reports unsupported sessions, inaccessible user manager, or filesystem errors.
pub fn apply(backend: LoginAutostart, home: &Path, exe: &Path) -> Result<()> {
    let config = config_home()?;
    let unit = config.join("systemd/user").join(UNIT);
    let desktop = config.join("autostart").join(DESKTOP);
    match backend {
        LoginAutostart::Systemd => {
            systemctl(&["show-environment"])
                .context("this architecture does not support systemd user services")?;
            write_owned(&unit, &unit_contents(exe, home)?)?;
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", UNIT])?;
            if desktop.exists() {
                std::fs::remove_file(&desktop)?;
            }
        }
        LoginAutostart::Kde | LoginAutostart::Gnome => {
            if !session_supports(backend) {
                bail!(
                    "this architecture/session does not support {} autostart",
                    backend.label()
                );
            }
            if unit.exists() {
                systemctl(&["disable", UNIT])?;
                std::fs::remove_file(&unit)?;
                systemctl(&["daemon-reload"])?;
            }
            write_owned(&desktop, &desktop_contents(exe, home, backend)?)?;
        }
        LoginAutostart::Off => {
            if unit.exists() {
                systemctl(&["disable", UNIT])?;
                std::fs::remove_file(&unit)?;
                systemctl(&["daemon-reload"])?;
            }
            if desktop.exists() {
                std::fs::remove_file(&desktop)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_entries_launch_the_core_without_a_terminal() {
        let exe = Path::new("/opt/my 100% app/cvt");
        let home = Path::new("/home/user/proxy data");
        let unit = unit_contents(exe, home).unwrap();
        assert!(unit.contains(
            "ExecStart=\"/opt/my 100%% app/cvt\" --home \"/home/user/proxy data\" core start"
        ));
        assert!(unit.contains("ExecStop="));
        let desktop = desktop_contents(exe, home, LoginAutostart::Gnome).unwrap();
        assert!(desktop.contains("OnlyShowIn=GNOME;"));
        assert!(desktop.contains("Terminal=false"));
        assert!(desktop.contains("/opt/my 100%% app/cvt"));
    }
}
