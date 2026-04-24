use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

const ENABLE_ENV: &str = "CODEX_TUI_PERF_LOG";
const THRESHOLD_ENV: &str = "CODEX_TUI_PERF_THRESHOLD_MS";
const DEFAULT_THRESHOLD: Duration = Duration::from_millis(5);

static ENABLED: OnceLock<bool> = OnceLock::new();
static THRESHOLD: OnceLock<Duration> = OnceLock::new();

pub(crate) struct PerfTimer {
    label: &'static str,
    start: Option<Instant>,
}

impl PerfTimer {
    pub(crate) fn start(label: &'static str) -> Self {
        Self {
            label,
            start: enabled().then(Instant::now),
        }
    }
}

impl Drop for PerfTimer {
    fn drop(&mut self) {
        let Some(start) = self.start else {
            return;
        };
        let elapsed = start.elapsed();
        if elapsed < threshold() {
            return;
        }
        tracing::info!(
            target: "codex_tui::perf",
            label = self.label,
            elapsed_us = elapsed.as_micros(),
            "tui perf timer exceeded threshold"
        );
    }
}

pub(crate) fn measure<T>(label: &'static str, f: impl FnOnce() -> T) -> T {
    let _timer = PerfTimer::start(label);
    f()
}

fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        let Ok(value) = std::env::var(ENABLE_ENV) else {
            return false;
        };
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn threshold() -> Duration {
    *THRESHOLD.get_or_init(|| {
        std::env::var(THRESHOLD_ENV)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_THRESHOLD)
    })
}
