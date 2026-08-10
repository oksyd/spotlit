use std::cell::RefCell;

use anyhow::Context;
use slint::{CloseRequestResponse, ComponentHandle};

use crate::{MainWindow, app_state::AppState, diagnostics, ui_events::UiSink, window_presenter};

thread_local! {
    static WINDOW_MANAGER: RefCell<Option<WindowManager>> = const { RefCell::new(None) };
}

pub(crate) fn install(app_state: AppState, ui_sink: UiSink) {
    WINDOW_MANAGER.with(|manager| {
        *manager.borrow_mut() = Some(WindowManager::new(app_state, ui_sink));
    });
    tracing::info!("window manager installed");
}

pub(crate) fn set_tray_available(available: bool) {
    WINDOW_MANAGER.with(|manager| {
        let mut manager = manager.borrow_mut();
        let Some(manager) = manager.as_mut() else {
            tracing::warn!("window manager was not installed");
            return;
        };

        manager.set_tray_available(available);
    });
}

pub(crate) fn present_window() {
    WINDOW_MANAGER.with(|manager| {
        let mut manager = manager.borrow_mut();
        let Some(manager) = manager.as_mut() else {
            tracing::warn!("window manager was not installed");
            return;
        };

        if let Err(error) = manager.present() {
            tracing::warn!(%error, "failed to present main window");
        }
    });
}

fn close_current_window(window_id: u64) -> CloseRequestResponse {
    WINDOW_MANAGER.with(|manager| {
        let mut manager = manager.borrow_mut();
        let Some(manager) = manager.as_mut() else {
            return CloseRequestResponse::HideWindow;
        };

        manager.close_window(window_id)
    })
}

struct WindowManager {
    app_state: AppState,
    ui_sink: UiSink,
    app: Option<MainWindow>,
    current_window_id: u64,
    next_window_id: u64,
    startup_marked: bool,
    tray_available: bool,
}

impl WindowManager {
    fn new(app_state: AppState, ui_sink: UiSink) -> Self {
        Self {
            app_state,
            ui_sink,
            app: None,
            current_window_id: 0,
            next_window_id: 1,
            startup_marked: false,
            tray_available: false,
        }
    }

    fn set_tray_available(&mut self, available: bool) {
        self.tray_available = available;
        tracing::info!(available, "tray availability updated");
    }

    fn present(&mut self) -> anyhow::Result<()> {
        tracing::info!(
            has_window = self.app.is_some(),
            window_id = self.current_window_id,
            "present window requested"
        );

        if self.app.is_none() {
            self.create_window()
                .context("failed to create main window")?;
        }

        let Some(app) = self.app.as_ref() else {
            return Ok(());
        };

        self.ui_sink.set_current(app);
        window_presenter::present(app);
        self.app_state
            .apply_cached_snapshot_after_present(app.as_weak());

        if !self.startup_marked {
            diagnostics::mark_since_start(diagnostics::Metric::Startup);
            self.startup_marked = true;
        }
        diagnostics::apply_to_ui(app);
        self.app_state.refresh_after_present(app.as_weak());
        Ok(())
    }

    fn create_window(&mut self) -> anyhow::Result<()> {
        let app = MainWindow::new().context("failed to create main window")?;
        let window_id = self.next_window_id;
        self.next_window_id = self.next_window_id.saturating_add(1);
        self.current_window_id = window_id;

        self.app_state.install(&app);
        self.install_close_handler(&app, window_id);
        self.ui_sink.set_current(&app);
        self.app = Some(app);
        tracing::info!(window_id, "main window created");
        Ok(())
    }

    fn install_close_handler(&self, app: &MainWindow, window_id: u64) {
        app.window().on_close_requested(move || {
            tracing::info!(window_id, "main window close requested");
            close_current_window(window_id)
        });
    }

    fn close_window(&mut self, window_id: u64) -> CloseRequestResponse {
        if self.current_window_id != window_id || self.app.is_none() {
            return CloseRequestResponse::HideWindow;
        }

        let keep_running_in_background = self
            .app_state
            .keep_running_in_background()
            .unwrap_or_else(|| {
                self.app
                    .as_ref()
                    .is_some_and(MainWindow::get_keep_running_in_background_enabled)
            });
        if should_hide_to_tray(self.tray_available, keep_running_in_background) {
            self.hide_window_to_tray(window_id);
        } else {
            self.quit_after_close(window_id);
        }

        CloseRequestResponse::HideWindow
    }

    fn hide_window_to_tray(&mut self, window_id: u64) {
        if let Some(app) = self.app.as_ref() {
            self.ui_sink.clear_current();
            self.app_state.cancel_window_work();
            window_presenter::cancel_pending_window_work();
            app.window().set_minimized(false);
            tracing::info!(window_id, "main window hidden to tray");
        }
    }

    fn quit_after_close(&mut self, window_id: u64) {
        if let Some(app) = self.app.as_ref() {
            self.ui_sink.clear_current();
            self.app_state.cancel_window_work();
            window_presenter::cancel_pending_window_work();
            crate::bridge::release_window_images(app);
            tracing::info!(window_id, "main window closed without tray");
        }

        if let Err(error) = slint::quit_event_loop() {
            tracing::warn!(%error, "failed to quit event loop after closing main window");
        }
    }
}

fn should_hide_to_tray(tray_available: bool, keep_running_in_background: bool) -> bool {
    tray_available && keep_running_in_background
}

#[cfg(test)]
mod tests {
    use super::should_hide_to_tray;

    #[test]
    fn close_hides_only_when_tray_and_background_mode_are_available() {
        assert!(should_hide_to_tray(true, true));
        assert!(!should_hide_to_tray(true, false));
        assert!(!should_hide_to_tray(false, true));
    }
}
