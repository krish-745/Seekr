//! Built-in latency telemetry, summarised to the log in batches when the panel hides.

use std::{sync::Mutex, time::Instant};

use tauri::State;

/// show: hotkey to panel ready; keystroke: to the first results painted; files: to file results.
const METRICS: [&str; 3] = ["show", "keystroke", "files"];
/// Samples per metric before a summary line is logged, so everyday use stays quiet.
const BATCH: usize = 50;

#[derive(Default)]
pub struct Telemetry {
    shown_at: Mutex<Option<Instant>>,
    samples: Mutex<Vec<(&'static str, f64)>>,
}

impl Telemetry {
    /// Starts the "show" clock: from the hotkey handler to the panel's webview having focus.
    pub fn mark_shown(&self) {
        *self.shown_at.lock().unwrap() = Some(Instant::now());
    }

    fn record(&self, metric: &'static str, ms: f64) {
        self.samples.lock().unwrap().push((metric, ms));
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

#[tauri::command]
pub fn panel_ready(telemetry: State<'_, Telemetry>) {
    if let Some(at) = telemetry.shown_at.lock().unwrap().take() {
        telemetry.record("show", at.elapsed().as_secs_f64() * 1000.0);
    }
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
}
