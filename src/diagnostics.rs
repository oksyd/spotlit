use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use slint::SharedString;

use crate::MainWindow;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Metric {
    Startup,
    FirstScan,
    FirstSnapshot,
    CurrentPreview,
    Thumbnails,
}

#[derive(Debug)]
struct Diagnostics {
    enabled: bool,
    process_started_at: Instant,
    startup: Option<Duration>,
    first_scan: Option<Duration>,
    first_snapshot: Option<Duration>,
    current_preview: Option<Duration>,
    thumbnails: Option<Duration>,
}

impl Diagnostics {
    fn new() -> Self {
        Self {
            enabled: cfg!(debug_assertions) || std::env::var_os("SPOTLIT_DIAGNOSTICS").is_some(),
            process_started_at: Instant::now(),
            startup: None,
            first_scan: None,
            first_snapshot: None,
            current_preview: None,
            thumbnails: None,
        }
    }

    fn set_metric(&mut self, metric: Metric, elapsed: Duration) -> bool {
        match metric {
            Metric::Startup => {
                self.startup = Some(elapsed);
                true
            }
            Metric::FirstScan => {
                if self.first_scan.is_some() {
                    false
                } else {
                    self.first_scan = Some(elapsed);
                    true
                }
            }
            Metric::FirstSnapshot => {
                if self.first_snapshot.is_some() {
                    false
                } else {
                    self.first_snapshot = Some(elapsed);
                    true
                }
            }
            Metric::CurrentPreview => {
                self.current_preview = Some(elapsed);
                true
            }
            Metric::Thumbnails => {
                self.thumbnails = Some(elapsed);
                true
            }
        }
    }

    fn metric(&self, metric: Metric) -> Option<Duration> {
        match metric {
            Metric::Startup => self.startup,
            Metric::FirstScan => self.first_scan,
            Metric::FirstSnapshot => self.first_snapshot,
            Metric::CurrentPreview => self.current_preview,
            Metric::Thumbnails => self.thumbnails,
        }
    }
}

static DIAGNOSTICS: OnceLock<Mutex<Diagnostics>> = OnceLock::new();

pub(crate) fn initialize() {
    let diagnostics = state();
    let enabled = diagnostics
        .lock()
        .map(|diagnostics| diagnostics.enabled)
        .unwrap_or(false);
    tracing::info!(enabled, "diagnostics initialized");
}

pub(crate) fn mark_since_start(metric: Metric) {
    let elapsed = state()
        .lock()
        .map(|diagnostics| diagnostics.process_started_at.elapsed())
        .unwrap_or_default();
    record(metric, elapsed);
}

pub(crate) fn record(metric: Metric, elapsed: Duration) {
    let updated = state()
        .lock()
        .map(|mut diagnostics| diagnostics.set_metric(metric, elapsed))
        .unwrap_or(false);

    if !updated {
        return;
    }

    tracing::info!(
        metric = metric_name(metric),
        elapsed_ms = elapsed.as_millis(),
        "performance metric"
    );
}

pub(crate) fn apply_to_ui(app: &MainWindow) {
    let Ok(diagnostics) = state().lock() else {
        return;
    };

    set_bool_if_changed(
        app.get_diagnostics_enabled(),
        diagnostics.enabled,
        |value| app.set_diagnostics_enabled(value),
    );

    if !diagnostics.enabled {
        return;
    }

    set_shared_string_if_changed(
        app.get_diagnostics_startup(),
        format_metric(diagnostics.metric(Metric::Startup)).into(),
        |value| app.set_diagnostics_startup(value),
    );
    set_shared_string_if_changed(
        app.get_diagnostics_first_scan(),
        format_metric(diagnostics.metric(Metric::FirstScan)).into(),
        |value| app.set_diagnostics_first_scan(value),
    );
    set_shared_string_if_changed(
        app.get_diagnostics_first_snapshot(),
        format_metric(diagnostics.metric(Metric::FirstSnapshot)).into(),
        |value| app.set_diagnostics_first_snapshot(value),
    );
    set_shared_string_if_changed(
        app.get_diagnostics_current_preview(),
        format_metric(diagnostics.metric(Metric::CurrentPreview)).into(),
        |value| app.set_diagnostics_current_preview(value),
    );
    set_shared_string_if_changed(
        app.get_diagnostics_thumbnails(),
        format_metric(diagnostics.metric(Metric::Thumbnails)).into(),
        |value| app.set_diagnostics_thumbnails(value),
    );
}

fn state() -> &'static Mutex<Diagnostics> {
    DIAGNOSTICS.get_or_init(|| Mutex::new(Diagnostics::new()))
}

fn metric_name(metric: Metric) -> &'static str {
    match metric {
        Metric::Startup => "startup",
        Metric::FirstScan => "first_scan",
        Metric::FirstSnapshot => "first_snapshot",
        Metric::CurrentPreview => "current_preview",
        Metric::Thumbnails => "thumbnails",
    }
}

fn format_metric(duration: Option<Duration>) -> String {
    duration
        .map(|duration| format!("{} ms", duration.as_millis()))
        .unwrap_or_else(|| "-".to_string())
}

fn set_bool_if_changed(current: bool, next: bool, setter: impl FnOnce(bool)) {
    if current != next {
        setter(next);
    }
}

fn set_shared_string_if_changed(
    current: SharedString,
    next: SharedString,
    setter: impl FnOnce(SharedString),
) {
    if current != next {
        setter(next);
    }
}
