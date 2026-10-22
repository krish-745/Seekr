//! Built-in latency telemetry, summarised to the log in batches when the panel hides.

use std::{collections::VecDeque, sync::Mutex, time::Instant};

use serde::Serialize;
use tauri::{AppHandle, State};

/// show: hotkey to panel ready; keystroke: to the first results painted; files and content: to
/// file-name and file-content results.
const METRICS: [&str; 4] = ["show", "keystroke", "files", "content"];
/// Samples per metric before a summary line is logged, so everyday use stays quiet.
const BATCH: usize = 50;
/// Recent samples kept for the diagnostics view, all metrics together.
const KEPT: usize = 2_000;
/// Histogram bucket upper bounds in ms; the last bucket is everything slower.
const BUCKETS: [f64; 6] = [5.0, 10.0, 16.0, 30.0, 50.0, 100.0];

/// One metric's recent latencies, for the diagnostics view.
#[derive(Serialize)]
pub struct Latency {
    metric: &'static str,
    n: usize,
    p50: f64,
    p95: f64,
    max: f64,
    /// Samples per bucket of BUCKETS, plus one for slower ones.
    buckets: Vec<usize>,
}

#[derive(Default)]
pub struct Telemetry {
    shown_at: Mutex<Option<Instant>>,
    samples: Mutex<Vec<(&'static str, f64)>>,
    recent: Mutex<VecDeque<(&'static str, f64)>>,
}

impl Telemetry {
    /// Starts the "show" clock: from the hotkey handler to the panel's webview having focus.
    pub fn mark_shown(&self) {
        *self.shown_at.lock().unwrap() = Some(Instant::now());
    }

    fn record(&self, metric: &'static str, ms: f64) {
        self.samples.lock().unwrap().push((metric, ms));
        let mut recent = self.recent.lock().unwrap();
        if recent.len() == KEPT {
            recent.pop_front();
        }
        recent.push_back((metric, ms));
    }

    /// Percentiles and a histogram of each metric's recent samples, since Seekr started.
    pub fn summary(&self) -> Vec<Latency> {
        let recent = self.recent.lock().unwrap();
        METRICS
            .iter()
            .filter_map(|&metric| {
                let mut ms: Vec<f64> = recent
                    .iter()
                    .filter(|(m, _)| *m == metric)
                    .map(|(_, v)| *v)
                    .collect();
                if ms.is_empty() {
                    return None;
                }
                ms.sort_by(f64::total_cmp);
                let mut buckets = vec![0; BUCKETS.len() + 1];
                for v in &ms {
                    buckets[BUCKETS.iter().position(|b| v <= b).unwrap_or(BUCKETS.len())] += 1;
                }
                Some(Latency {
                    metric,
                    n: ms.len(),
                    p50: percentile(&ms, 0.50),
                    p95: percentile(&ms, 0.95),
                    max: ms[ms.len() - 1],
                    buckets,
                })
            })
            .collect()
    }

    /// Logs a summary for each metric that has collected a full batch, keeping the rest.
    pub fn flush(&self) {
        let mut samples = self.samples.lock().unwrap();
        for metric in METRICS {
            let mut ms: Vec<f64> = samples
                .iter()
                .filter(|(m, _)| *m == metric)
                .map(|(_, v)| *v)
                .collect();
            if ms.len() < BATCH {
                continue;
            }
            samples.retain(|(m, _)| *m != metric);
            ms.sort_by(f64::total_cmp);
            tracing::info!(
                "latency {metric}: n={} p50={:.1} p95={:.1} max={:.1} ms",
                ms.len(),
                percentile(&ms, 0.50),
                percentile(&ms, 0.95),
                ms[ms.len() - 1]
            );
        }
    }
}

/// Nearest-rank percentile of an ascending, non-empty slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// The panel is on screen and focused; also warms the meaning-search model for typing and
/// rereads browser bookmarks and history if they are stale.
#[tauri::command]
pub fn panel_ready(app: AppHandle, telemetry: State<'_, Telemetry>) {
    if let Some(at) = telemetry.shown_at.lock().unwrap().take() {
        telemetry.record("show", at.elapsed().as_secs_f64() * 1000.0);
    }
    crate::content::warm(&app);
    crate::web::refresh(&app);
}

/// Keystroke-to-painted time measured in the webview.
#[tauri::command]
pub fn record_latency(metric: String, ms: f64, telemetry: State<'_, Telemetry>) {
    match METRICS.iter().find(|m| **m == metric) {
        Some(m) if ms.is_finite() && ms >= 0.0 => telemetry.record(m, ms),
        _ => tracing::warn!("ignored latency sample {metric}={ms}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let ms: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&ms, 0.50), 10.0);
        assert_eq!(percentile(&ms, 0.95), 19.0);
        assert_eq!(percentile(&[7.0], 0.95), 7.0);
    }

    #[test]
    fn flush_waits_for_a_full_batch() {
        let t = Telemetry::default();
        for i in 0..BATCH {
            t.record("keystroke", i as f64);
        }
        t.record("show", 1.0);
        t.flush();
        assert_eq!(*t.samples.lock().unwrap(), [("show", 1.0)]);
    }

    #[test]
    fn summary_buckets_recent_samples() {
        let t = Telemetry::default();
        for ms in [3.0, 12.0, 15.0, 250.0] {
            t.record("files", ms);
        }
        let summary = t.summary();
        assert_eq!(summary.len(), 1);
        let files = &summary[0];
        assert_eq!((files.metric, files.n, files.max), ("files", 4, 250.0));
        assert_eq!(files.buckets, [1, 0, 2, 0, 0, 0, 1]);
        for i in 0..KEPT + 5 {
            t.record("show", i as f64);
        }
        assert_eq!(t.recent.lock().unwrap().len(), KEPT);
    }
}
