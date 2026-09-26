//! `unlock` — whether the exit can reach the services people actually want.
//!
//! A latency number says a socket opened. It does not say whether the other end
//! will serve you: a node can be fast, in the right country, and on a
//! datacentre range that the streaming services have already blocked, and the
//! only way to know is to ask them.
//!
//! Every verdict here is a *reading of a public response*, and the response is
//! printed beside it. That is deliberate. A tool that says "Netflix: unlocked"
//! and cannot show why is a tool whose answer cannot be checked, and this one
//! is wrong often enough — services change their pages, and a probe URL that
//! worked last month may answer a login wall today — that being able to see the
//! evidence is the difference between a reading and a guess.

use anyhow::Result;
use serde::Serialize;

use crate::cli::UnlockArgs;
use crate::commands::{check_request_timeout, proxied_client, proxy_port};
use crate::context::Ctx;
use crate::output::{Output, Report, Table};

/// How much of a page is read to find a signal in it.
const BODY_LIMIT: usize = 64 * 1024;

/// Read at most `limit` bytes of a response body.
async fn read_prefix(response: reqwest::Response, limit: usize) -> String {
    use futures_util::StreamExt as _;
    let mut body: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(Ok(chunk)) = stream.next().await {
        let room = limit.saturating_sub(body.len());
        if room == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
    String::from_utf8_lossy(&body).into_owned()
}

/// What one probe concluded, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The service answered as it does for a supported region.
    Unlocked,
    /// The service answered as it does for an unsupported one.
    Blocked,
    /// The answer says nothing either way — a login wall, a redesign, a
    /// redirect. Reported as itself rather than rounded to one of the other
    /// two, because a guess is worse than a blank.
    Unknown,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Self::Unlocked => "unlocked",
            Self::Blocked => "blocked",
            Self::Unknown => "unknown",
        }
    }
}

/// One service and how to ask it.
struct Probe {
    name: &'static str,
    url: &'static str,
}

/// The services worth asking about, and the URL that answers.
///
/// Chosen because each one *does* answer differently by region, which is the
/// only property that makes a probe meaningful. A URL that returns the same
/// page everywhere would report `unlocked` from a country where the service
/// does not operate.
const PROBES: &[Probe] = &[
    Probe {
        name: "youtube-premium",
        url: "https://www.youtube.com/premium",
    },
    Probe {
        name: "netflix",
        // A title that is licensed in some regions and not others; the status
        // is the whole signal.
        url: "https://www.netflix.com/title/81280792",
    },
    Probe {
        name: "chatgpt",
        // Answered 200 with an empty session object where the service operates,
        // and 403 where it does not.
        url: "https://chatgpt.com/api/auth/session",
    },
    Probe {
        name: "disney-plus",
        url: "https://www.disneyplus.com/",
    },
];

/// One service's result.
#[derive(Debug, Serialize)]
pub struct UnlockRow {
    /// The service, as named above.
    pub service: String,
    /// What the response was read as.
    pub verdict: Verdict,
    /// The HTTP status, when there was one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Why the verdict is what it is, in one line, quoting the response.
    pub evidence: String,
    /// Round trip in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
}

/// What `unlock` found.
#[derive(Debug, Serialize)]
pub struct UnlockReport {
    /// How the requests were made.
    pub via: String,
    /// One per service, in the order above.
    pub rows: Vec<UnlockRow>,
    /// The library's one-line summary.
    pub summary: String,
}

impl Report for UnlockReport {
    fn schema(&self) -> &'static str {
        "cvt.unlock.v1"
    }

    fn render(&self, _out: Output) -> String {
        let mut table = Table::new(["service", "verdict", "evidence"]);
        for row in &self.rows {
            table.push([
                row.service.clone(),
                row.verdict.label().to_owned(),
                row.evidence.clone(),
            ]);
        }
        table.render()
    }
}

/// Run `unlock`.
///
/// # Errors
/// `Error::InvalidValue` when there is no generated configuration to read a
/// proxy port from.
pub async fn run(ctx: &Ctx, args: &UnlockArgs) -> Result<()> {
    let timeout = check_request_timeout(args.timeout.unwrap_or(15_000))?;
    let port = proxy_port(ctx)?;
    let client = proxied_client(Some(port), timeout)?;
    let via = format!("the core's proxy on 127.0.0.1:{port}");

    let mut rows = Vec::new();
    for probe in PROBES {
        rows.push(ask(&client, probe).await);
    }
    let unlocked = rows
        .iter()
        .filter(|row| row.verdict == Verdict::Unlocked)
        .count();
    let summary = format!(
        "{unlocked} of {} service(s) reachable through {via}",
        rows.len()
    );
    ctx.out().emit(&UnlockReport { via, rows, summary })
}

/// Ask one service, and read the answer.
async fn ask(client: &reqwest::Client, probe: &Probe) -> UnlockRow {
    let started = std::time::Instant::now();
    let response = match client.get(probe.url).send().await {
        Ok(response) => response,
        Err(error) => {
            return UnlockRow {
                service: probe.name.to_owned(),
                verdict: Verdict::Unknown,
                status: None,
                evidence: format!("no answer: {error}"),
                delay_ms: None,
            };
        }
    };
    let status = response.status();
    let delay_ms = u64::try_from(started.elapsed().as_millis()).ok();
    // Bounded: these are pages, and only a prefix is needed to read a signal.
    // The crate already had a bound for the subscription fetcher and this
    // command stated one it did not have.
    let body = read_prefix(response, BODY_LIMIT).await;
    let (verdict, evidence) = read(probe.name, status.as_u16(), &body);
    UnlockRow {
        service: probe.name.to_owned(),
        verdict,
        status: Some(status.as_u16()),
        evidence,
        delay_ms,
    }
}

/// The reading, per service, with the evidence that produced it.
fn read(service: &str, status: u16, body: &str) -> (Verdict, String) {
    // Read once, and consulted *inside* every arm whose signal is the status.
    // A login wall arrives as `HTTP 200` — the redirect is followed and the
    // reading is handed the login page — so an arm that turns any 200 into
    // `Unlocked` reports the service as available when it is asking you to sign
    // in. `docs/CLI.md` promises the opposite in as many words.
    let walled = is_a_wall(body);
    let wall = || {
        (
            Verdict::Unknown,
            format!("HTTP {status}: a login wall or a notice, not the service"),
        )
    };
    match service {
        "youtube-premium" => {
            if walled {
                return wall();
            }
            // The page carries the country it decided you are in. Where Premium
            // is not sold the same URL answers without it.
            if let Some(code) = between(body, "\"countryCode\":\"", "\"") {
                (Verdict::Unlocked, format!("HTTP {status}, region {code}"))
            } else if status == 200 {
                (
                    Verdict::Blocked,
                    "HTTP 200 without a region on the Premium page".to_owned(),
                )
            } else {
                (Verdict::Unknown, format!("HTTP {status}"))
            }
        }
        // The status is the signal, and the *body* decides whether the status
        // is the service's. Matching on the pair rather than on the status
        // alone is what makes the wall case impossible to forget in an arm: a
        // `200 =>` with nothing beside it cannot be written.
        "netflix" => match (status, walled) {
            (_, true) => wall(),
            (200, false) => (
                Verdict::Unlocked,
                "HTTP 200 for a region-locked title".to_owned(),
            ),
            (404, false) => (
                Verdict::Blocked,
                "HTTP 404: the title is not licensed here".to_owned(),
            ),
            _ => (Verdict::Unknown, format!("HTTP {status}")),
        },
        "chatgpt" => match (status, walled) {
            (_, true) => wall(),
            (200, false) => (
                Verdict::Unlocked,
                "HTTP 200 from the session endpoint".to_owned(),
            ),
            (403, false) => (
                Verdict::Blocked,
                "HTTP 403: not available in this country".to_owned(),
            ),
            _ => (Verdict::Unknown, format!("HTTP {status}")),
        },
        "disney-plus" => match (status, walled) {
            (_, true) => wall(),
            (200, false) => (Verdict::Unlocked, "HTTP 200".to_owned()),
            // Disney answers a redirect to a "not available" page rather than a
            // 4xx in most regions it does not serve.
            (301 | 302 | 403, false) => (
                Verdict::Blocked,
                format!("HTTP {status}: redirected away from the service"),
            ),
            _ => (Verdict::Unknown, format!("HTTP {status}")),
        },
        _ if walled => wall(),
        _ => (Verdict::Unknown, format!("HTTP {status}")),
    }
}

/// Whether a 200 is really a wall rather than the service.
///
/// Matched on phrases these pages actually use. Deliberately a short list and a
/// conservative one: a false positive turns a working service into `unknown`,
/// which is a smaller lie than the reverse but still a lie.
fn is_a_wall(body: &str) -> bool {
    const MARKERS: &[&str] = &[
        "sign in to continue",
        "log in to continue",
        "please log in",
        "please sign in",
        "too many requests",
        "rate limit",
        "captcha",
        "unusual traffic",
        "not available in your",
    ];
    let lower = body.to_ascii_lowercase();
    MARKERS.iter().any(|marker| lower.contains(marker))
}

/// The text between two markers, when both are present.
fn between(haystack: &str, open: &str, close: &str) -> Option<String> {
    let start = haystack.find(open)? + open.len();
    let rest = &haystack[start..];
    let end = rest.find(close)?;
    Some(rest[..end].to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_region_on_the_page_is_the_youtube_signal() {
        let body = r#"{"countryCode":"JP","premium":true}"#;
        let (verdict, evidence) = read("youtube-premium", 200, body);
        assert_eq!(verdict, Verdict::Unlocked);
        assert!(evidence.contains("JP"), "{evidence}");

        // The same URL in a country where it is not sold answers 200 without a
        // region, which is the case worth telling apart from a network error.
        let (verdict, evidence) = read("youtube-premium", 200, "<html>no region</html>");
        assert_eq!(verdict, Verdict::Blocked);
        assert!(evidence.contains("without a region"), "{evidence}");
    }

    #[test]
    fn a_status_is_the_whole_signal_where_it_is_one() {
        assert_eq!(read("netflix", 200, "").0, Verdict::Unlocked);
        assert_eq!(read("netflix", 404, "").0, Verdict::Blocked);
        assert_eq!(read("chatgpt", 200, "{}").0, Verdict::Unlocked);
        assert_eq!(read("chatgpt", 403, "").0, Verdict::Blocked);
    }

    #[test]
    fn an_answer_that_says_nothing_is_reported_as_such() {
        // A login wall, a rate limit, a redesign: none of them is a verdict,
        // and rounding one to `blocked` would be a guess presented as a reading.
        for (service, status) in [
            ("netflix", 500),
            ("chatgpt", 429),
            ("disney-plus", 418),
            ("youtube-premium", 503),
            ("something-new", 200),
        ] {
            assert_eq!(
                read(service, status, "").0,
                Verdict::Unknown,
                "{service} {status} is not a verdict"
            );
        }
    }

    #[test]
    fn the_marker_reader_is_not_fooled_by_a_missing_half() {
        assert_eq!(between("a\"b\"c", "\"", "\""), Some("b".to_owned()));
        assert_eq!(between("no markers", "\"", "\""), None);
        assert_eq!(between("\"unclosed", "\"", "\""), None);
    }
}
