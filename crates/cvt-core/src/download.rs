//! Bounded HTTPS artifact downloads with explicit routing and fallback.
//!
//! Passing the owned running core's HTTP proxy avoids resolving GitHub domains
//! through the host resolver when TUN supplies Fake-IP answers. It still obeys
//! the core's routing rules; this adapter never disables TLS verification.

use std::time::Duration;

use crate::{Error, Result};

/// Reusable transport for public release metadata and archive downloads.
#[derive(Debug, Clone)]
pub struct ArtifactFetcher {
    routes: Vec<(&'static str, reqwest::Client)>,
}

impl ArtifactFetcher {
    /// Prefer the running core, then environment proxies, then explicit direct.
    /// Each request has a connect, read and total timeout.
    ///
    /// # Errors
    /// Returns an HTTP setup error for an invalid proxy or client configuration.
    pub fn new(core_proxy: Option<&str>) -> Result<Self> {
        let builder = || {
            reqwest::Client::builder()
                .user_agent(concat!("clash-verge-tui/", env!("CARGO_PKG_VERSION")))
                .https_only(true)
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(20))
                .timeout(Duration::from_secs(90))
        };
        let mut routes = Vec::new();
        if let Some(proxy) = core_proxy {
            let proxy =
                reqwest::Proxy::all(proxy).map_err(|e| Error::http("core download proxy", e))?;
            routes.push((
                "running core proxy",
                builder()
                    .no_proxy()
                    .proxy(proxy)
                    .build()
                    .map_err(|e| Error::http("core download proxy", e))?,
            ));
        }
        routes.push((
            "environment proxy",
            builder()
                .build()
                .map_err(|e| Error::http("environment proxy", e))?,
        ));
        routes.push((
            "direct",
            builder()
                .no_proxy()
                .build()
                .map_err(|e| Error::http("direct", e))?,
        ));
        Ok(Self { routes })
    }

    /// Fetch a bounded body, retrying another route after transport/status errors.
    /// Successful bytes still require the caller's format/digest verification.
    ///
    /// # Errors
    /// Reports all failed routes, or refuses an oversized body without retrying.
    pub async fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>> {
        let mut failures = Vec::new();
        for (label, client) in &self.routes {
            let result = async {
                let mut response = client
                    .get(url)
                    .header(
                        "Accept",
                        if url.starts_with("https://api.github.com/") {
                            "application/vnd.github+json"
                        } else {
                            "application/octet-stream"
                        },
                    )
                    .send()
                    .await
                    .map_err(|e| Error::http(url, e))?
                    .error_for_status()
                    .map_err(|e| Error::http(url, e))?;
                if response
                    .content_length()
                    .is_some_and(|size| size > limit as u64)
                {
                    return Err(Error::invalid(
                        "download size",
                        "response exceeds its size limit",
                    ));
                }
                let mut body = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(|e| Error::http(url, e))? {
                    if chunk.len() > limit.saturating_sub(body.len()) {
                        return Err(Error::invalid(
                            "download size",
                            "response exceeds its size limit",
                        ));
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(body)
            }
            .await;
            match result {
                Ok(body) => return Ok(body),
                Err(error @ Error::InvalidValue { .. }) => return Err(error),
                Err(error) => failures.push(format!("{label}: {error}")),
            }
        }
        Err(Error::CoreUnavailable {
            reason: format!("download failed via all routes: {}", failures.join("; ")),
        })
    }
}
