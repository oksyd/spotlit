mod app_state;
mod bridge;
mod command;
mod core;
mod diagnostics;
mod i18n;
mod image_cache;
mod logging;
mod options;
mod platform;
mod preview_image;
mod runtime;
mod tray;
mod ui_events;
mod update;
mod window_manager;
mod window_presenter;
mod worker;
mod worker_event;
mod worker_runtime;
#[cfg(test)]
mod worker_tests;

pub use runtime::{run, run_from_env};

slint::include_modules!();
