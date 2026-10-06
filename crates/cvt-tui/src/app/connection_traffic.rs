//! Per-connection throughput derived from successive controller snapshots.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::row::ConnectionRow;
use crate::state::human_bytes;

use super::App;

#[derive(Debug, Default)]
pub(super) struct ConnectionTraffic {
    samples: HashMap<String, Sample>,
}

#[derive(Debug)]
struct Sample {
    upload: u64,
    download: u64,
    at: Instant,
    rates: Option<(u64, u64)>,
}

fn bytes_per_second(bytes: u64, elapsed: Duration) -> u64 {
    let rate = u128::from(bytes) * 1_000_000_000 / elapsed.as_nanos().max(1);
    u64::try_from(rate).unwrap_or(u64::MAX)
}

impl ConnectionTraffic {
    pub(super) fn update(&mut self, rows: &[ConnectionRow]) {
        let now = Instant::now();
        let mut samples = HashMap::with_capacity(rows.len());
        for row in rows {
            let sample = match self.samples.remove(&row.id) {
                // HTTP refreshes can arrive beside stream snapshots. Keep the
                // baseline until enough time has elapsed for a useful rate.
                Some(previous)
                    if now.duration_since(previous.at) < Duration::from_millis(100)
                        && row.upload >= previous.upload
                        && row.download >= previous.download =>
                {
                    previous
                }
                previous => {
                    let rates = previous.and_then(|previous| {
                        if row.upload < previous.upload || row.download < previous.download {
                            return None;
                        }
                        let elapsed = now.duration_since(previous.at);
                        Some((
                            bytes_per_second(row.upload - previous.upload, elapsed),
                            bytes_per_second(row.download - previous.download, elapsed),
                        ))
                    });
                    Sample {
                        upload: row.upload,
                        download: row.download,
                        at: now,
                        rates,
                    }
                }
            };
            samples.insert(row.id.clone(), sample);
        }
        self.samples = samples;
    }
}

impl App {
    pub(crate) fn connection_rate_label(&self, id: &str) -> String {
        self.connection_traffic
            .samples
            .get(id)
            .and_then(|sample| sample.rates)
            .map_or_else(
                || "—".to_owned(),
                |(upload, download)| {
                    format!("↑{}/s ↓{}/s", human_bytes(upload), human_bytes(download))
                },
            )
    }

    pub(super) fn connection_rate_total(&self, id: &str) -> u64 {
        self.connection_traffic
            .samples
            .get(id)
            .and_then(|sample| sample.rates)
            .map_or(0, |(upload, download)| upload.saturating_add(download))
    }
}
