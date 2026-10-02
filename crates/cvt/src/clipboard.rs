//! Explicit user-initiated copies, using desktop tools or terminal OSC 52.

use std::io::{IsTerminal as _, Write as _};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Result, bail};
use base64::Engine as _;
use tokio::io::AsyncWriteExt as _;

/// True means a terminal request was sent; terminals may refuse OSC 52.
pub async fn copy(text: &str) -> Result<bool> {
    if text.len() > 1024 * 1024 {
        bail!("copy exceeds the 1 MiB clipboard limit");
    }
    // On SSH, use the user's terminal clipboard, never the remote desktop.
    let remote =
        std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
    if !remote {
        let candidates: &[(&str, &[&str])] = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            &[
                ("wl-copy", &["--type", "text/plain;charset=utf-8"]),
                ("xclip", &["-selection", "clipboard"]),
                ("xsel", &["--clipboard", "--input"]),
            ]
        } else if cfg!(target_os = "macos") {
            &[("pbcopy", &[])]
        } else if std::env::var_os("DISPLAY").is_some() {
            &[
                ("xclip", &["-selection", "clipboard"]),
                ("xsel", &["--clipboard", "--input"]),
            ]
        } else {
            &[]
        };
        for (program, args) in candidates {
            let operation = async {
                let mut child = tokio::process::Command::new(program)
                    .args(*args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()?;
                let mut stdin = child
                    .stdin
                    .take()
                    .ok_or_else(|| std::io::Error::other("clipboard stdin unavailable"))?;
                stdin.write_all(text.as_bytes()).await?;
                drop(stdin);
                child.wait().await
            };
            if matches!(tokio::time::timeout(Duration::from_secs(3), operation).await,
                Ok(Ok(status)) if status.success())
            {
                return Ok(false);
            }
        }
    }
    if !std::io::stdout().is_terminal() {
        bail!("clipboard requires a desktop clipboard tool or an OSC 52 terminal");
    }
    // Base64 prevents copied logs/configuration from injecting terminal escapes.
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut stdout = std::io::stdout().lock();
    write!(stdout, "\x1b]52;c;{encoded}\x07")?;
    stdout.flush()?;
    Ok(true)
}
