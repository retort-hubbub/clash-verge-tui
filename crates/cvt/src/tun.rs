//! Explicit capabilities on the selected Linux core binary.
//! Authentication is performed with a desktop agent or on a restored terminal.

use std::io::IsTerminal as _;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use cvt_core::model::config::Config;

fn capability_tool(name: &str) -> Result<String> {
    [format!("/usr/bin/{name}"), format!("/usr/sbin/{name}")]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .ok_or_else(|| anyhow::anyhow!("{name} is unavailable; install libcap tools"))
}

/// Capabilities required by this generated document; remote server ports do
/// not require local listener privileges.
pub fn required_capabilities(config: &Config) -> String {
    if !cfg!(target_os = "linux") {
        return String::new();
    }
    let value = config.as_value();
    let threshold = std::fs::read_to_string("/proc/sys/net/ipv4/ip_unprivileged_port_start")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(1024);
    let low = |port: u64| port > 0 && port < threshold;
    let address_low = |address: &str| {
        address
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse().ok())
            .is_some_and(low)
    };
    let low_listener = [
        "port",
        "socks-port",
        "mixed-port",
        "redir-port",
        "tproxy-port",
    ]
    .iter()
    .any(|key| value[*key].as_u64().is_some_and(low))
        || (value["dns"]["enable"].as_bool() == Some(true)
            && value["dns"]["listen"].as_str().is_some_and(address_low))
        || value["external-controller"]
            .as_str()
            .is_some_and(address_low)
        || value["listeners"].as_array().is_some_and(|listeners| {
            listeners
                .iter()
                .any(|listener| listener["port"].as_u64().is_some_and(low))
        });
    let mut capabilities = Vec::new();
    if value["tun"]["enable"].as_bool() == Some(true) {
        capabilities.extend(["cap_net_admin", "cap_net_raw"]);
    }
    if low_listener {
        capabilities.push("cap_net_bind_service");
    }
    capabilities.join(",")
}

/// Inspect the effective and permitted capabilities without authenticating.
pub fn has_capabilities(binary: &Path, capabilities: &str) -> Result<bool> {
    if capabilities.is_empty() {
        return Ok(true);
    }
    let granted = file_capabilities(binary)?;
    Ok(capabilities_include(&granted, capabilities))
}

fn capabilities_include(granted: &str, capabilities: &str) -> bool {
    capabilities.split(',').all(|required| {
        let mut effective = false;
        let mut permitted = false;
        for clause in granted.split_whitespace() {
            let Some(index) = clause.find(['=', '+', '-']) else {
                continue;
            };
            let names = &clause[..index];
            let operator = clause.as_bytes()[index];
            let flags = &clause[index + 1..];
            if names.is_empty() || names.split(',').any(|name| name == required) {
                if operator == b'=' {
                    effective = flags.contains('e');
                    permitted = flags.contains('p');
                } else {
                    if flags.contains('e') {
                        effective = operator == b'+';
                    }
                    if flags.contains('p') {
                        permitted = operator == b'+';
                    }
                }
            }
        }
        effective && permitted
    })
}

// getcap does not follow symlinks. Preserve all clauses, including capability
// sets with different flags, when adding privileges to an existing binary.
fn file_capabilities(binary: &Path) -> Result<String> {
    let binary = std::fs::canonicalize(binary).context("locating the Mihomo binary")?;
    let output = Command::new(capability_tool("getcap")?)
        .arg(&binary)
        .output()?;
    if !output.status.success() {
        bail!("could not inspect Mihomo network capabilities");
    }
    let text = String::from_utf8(output.stdout).context("reading file capabilities")?;
    Ok(text
        .trim()
        .strip_prefix(binary.to_string_lossy().as_ref())
        .unwrap_or_default()
        .trim()
        .to_owned())
}

/// Grant the requested network capabilities after explicit user approval.
/// The caller must release the terminal before this may ask for a password.
pub fn authorize_capabilities(binary: &Path, capabilities: &str) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("core authorization is only supported on Linux");
    }
    if capabilities.is_empty()
        || capabilities
            .split(',')
            .any(|cap| !["cap_net_admin", "cap_net_raw", "cap_net_bind_service"].contains(&cap))
    {
        bail!("invalid network capability request");
    }
    let binary = std::fs::canonicalize(binary).context("locating the Mihomo binary")?;
    if !binary.is_file() {
        bail!("the Mihomo binary is not a regular file");
    }
    let resolver_required = capabilities.split(',').any(|cap| cap == "cap_net_admin")
        && cvt_core::mihomo::resolver::available();
    if has_capabilities(&binary, capabilities)?
        && (!resolver_required || cvt_core::mihomo::resolver::authorized())
    {
        return Ok(());
    }
    let helper = std::env::current_exe().context("locating the authorization helper")?;
    let helper = helper
        .to_str()
        .context("authorization executable path encoding")?;
    let uid = crate::resolver_authorization::uid()
        .context("determining the authenticated user")?
        .to_string();
    let desktop =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    let pkexec = ["/usr/bin/pkexec", "/bin/pkexec"]
        .into_iter()
        .find(|path| Path::new(path).is_file());
    let root = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("Uid:"))
                .and_then(|line| line.split_whitespace().nth(2))
                .map(|uid| uid == "0")
        })
        .unwrap_or(false);
    let use_pkexec = !root && desktop && pkexec.is_some();
    let mut command = authorization_command(
        helper,
        root,
        desktop,
        pkexec,
        std::io::stdin().is_terminal(),
    );
    let mut status = command
        .arg("--internal-authorize-core")
        .arg(&binary)
        .arg(capabilities)
        .arg(&uid)
        .status()
        .context("authorization needs pkexec on a desktop, or sudo in a terminal")?;
    if use_pkexec && status.code() == Some(127) && std::io::stdin().is_terminal() {
        // No policy agent in an SSH/headless session: allow terminal sudo.
        status = sudo_command(true)
            .arg(helper)
            .arg("--internal-authorize-core")
            .arg(&binary)
            .arg(capabilities)
            .arg(&uid)
            .status()?;
    }
    if !status.success() {
        bail!(
            "core authorization failed or was cancelled; retry authorization for network capabilities and scoped system DNS access"
        );
    }
    if !has_capabilities(&binary, capabilities)? {
        bail!("network capability grant did not take effect");
    }
    if resolver_required {
        // polkit reloads rules asynchronously after the atomic replacement.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !cvt_core::mihomo::resolver::authorized() {
            if std::time::Instant::now() >= deadline {
                bail!("the scoped systemd-resolved authorization did not take effect");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    Ok(())
}

fn sudo_command(terminal: bool) -> Command {
    let mut command = Command::new("sudo");
    if !terminal {
        command.arg("-n");
    }
    command.arg("--");
    command
}

fn authorization_command(
    setcap: &str,
    root: bool,
    desktop: bool,
    pkexec: Option<&str>,
    terminal: bool,
) -> Command {
    if root {
        return Command::new(setcap);
    }
    let mut command = if let Some(pkexec) = pkexec.filter(|_| desktop) {
        let mut command = Command::new(pkexec);
        command.arg("--disable-internal-agent");
        command
    } else {
        sudo_command(terminal)
    };
    command.arg(setcap);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_grants_are_found_across_all_getcap_clauses() {
        assert!(capabilities_include(
            "cap_net_admin,cap_net_raw=ep cap_chown=i",
            "cap_net_admin,cap_net_raw"
        ));
        assert!(capabilities_include("=ep cap_net_raw-e", "cap_net_admin"));
        assert!(!capabilities_include("=ep cap_net_raw-e", "cap_net_raw"));
        assert!(!capabilities_include("cap_net_admin=p", "cap_net_admin"));
    }

    #[test]
    fn headless_authorization_uses_sudo_even_when_pkexec_is_installed() {
        let command = authorization_command(
            "/usr/sbin/setcap",
            false,
            false,
            Some("/usr/bin/pkexec"),
            true,
        );
        assert_eq!(command.get_program(), "sudo");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--", "/usr/sbin/setcap"]
        );
        let command = authorization_command("/usr/sbin/setcap", false, true, None, true);
        assert_eq!(command.get_program(), "sudo");
        let command = authorization_command("/usr/sbin/setcap", false, false, None, false);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-n", "--", "/usr/sbin/setcap"]
        );
    }

    #[test]
    fn desktop_authorization_uses_agent_and_root_needs_no_wrapper() {
        let command = authorization_command(
            "/usr/sbin/setcap",
            false,
            true,
            Some("/usr/bin/pkexec"),
            true,
        );
        assert_eq!(command.get_program(), "/usr/bin/pkexec");
        let command = authorization_command(
            "/usr/sbin/setcap",
            true,
            true,
            Some("/usr/bin/pkexec"),
            true,
        );
        assert_eq!(command.get_program(), "/usr/sbin/setcap");
        assert_eq!(command.get_args().count(), 0);
    }

    #[test]
    fn enabled_low_port_dns_requests_bind_capability_and_tun_requests_network_caps() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let threshold: u64 =
            std::fs::read_to_string("/proc/sys/net/ipv4/ip_unprivileged_port_start")
                .unwrap()
                .trim()
                .parse()
                .unwrap();
        let config = Config::from_yaml(
            "dns: {enable: true, listen: ':53', enhanced-mode: fake-ip}\ntun: {enable: false}\n",
        )
        .unwrap();
        assert_eq!(
            required_capabilities(&config),
            if threshold > 53 {
                "cap_net_bind_service"
            } else {
                ""
            }
        );
        let config =
            Config::from_yaml("dns: {enable: false, listen: ':53'}\ntun: {enable: true}\n")
                .unwrap();
        assert_eq!(required_capabilities(&config), "cap_net_admin,cap_net_raw");
    }
}
