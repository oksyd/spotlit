use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[cfg(windows)]
use crate::platform::windows::{
    NativeWindowHandle, force_window_redraw, restore_window, window_is_visible,
};
#[cfg(windows)]
use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{ComponentHandle, winit_030::WinitWindowAccessor};

use crate::{MainWindow, ui_events};

static WINDOW_LIFECYCLE_GENERATION: AtomicU64 = AtomicU64::new(0);
const FIRST_REDRAW_DELAY: Duration = Duration::from_millis(16);
const IMAGE_WORK_DELAY: Duration = Duration::from_millis(16);
const IMAGE_WORK_RETRY_DELAY: Duration = Duration::from_millis(96);
const IMAGE_WORK_RETRY_COUNT: u8 = 2;
const SECOND_REDRAW_DELAY: Duration = Duration::from_millis(120);

pub(crate) fn present(app: &MainWindow) {
    let generation = next_lifecycle_generation();
    ui_events::set_window_accepts_image_work(false);

    if !app.window().is_visible()
        && let Err(error) = app.show()
    {
        tracing::warn!(%error, "failed to present main window");
        ui_events::set_window_accepts_image_work(true);
        return;
    }

    restore_native_window(app);

    app.window().set_minimized(false);
    request_redraw_and_focus(app);

    let app = app.as_weak();
    schedule_restore_redraw(app.clone(), generation, FIRST_REDRAW_DELAY, true);
    schedule_visible_image_work(
        app.clone(),
        generation,
        IMAGE_WORK_DELAY,
        IMAGE_WORK_RETRY_COUNT,
    );
    schedule_restore_redraw(app, generation, SECOND_REDRAW_DELAY, false);
}

pub(crate) fn cancel_pending_window_work() {
    next_lifecycle_generation();
    ui_events::suspend_image_work();
}

fn schedule_restore_redraw(
    app: slint::Weak<MainWindow>,
    generation: u64,
    delay: Duration,
    focus: bool,
) {
    slint::Timer::single_shot(delay, move || {
        if generation != current_lifecycle_generation() {
            return;
        }

        let Some(app) = app.upgrade() else {
            return;
        };

        if !native_window_is_visible(&app) {
            return;
        }

        if focus {
            request_redraw_and_focus(&app);
        } else {
            request_redraw(&app);
        }
    });
}

fn schedule_visible_image_work(
    app: slint::Weak<MainWindow>,
    generation: u64,
    delay: Duration,
    retries: u8,
) {
    slint::Timer::single_shot(delay, move || {
        if generation != current_lifecycle_generation() {
            return;
        }

        let Some(app) = app.upgrade() else {
            return;
        };

        if !native_window_is_visible(&app) {
            if retries > 0 {
                schedule_visible_image_work(
                    app.as_weak(),
                    generation,
                    IMAGE_WORK_RETRY_DELAY,
                    retries - 1,
                );
            }
            return;
        }

        ui_events::set_window_accepts_image_work(true);
        request_redraw(&app);
        ui_events::request_visible_images(&app);
    });
}

fn request_redraw_and_focus(app: &MainWindow) {
    force_native_window_redraw(app);
    app.window().with_winit_window(|window| {
        window.focus_window();
        window.request_redraw();
    });
    app.window().request_redraw();
}

fn request_redraw(app: &MainWindow) {
    force_native_window_redraw(app);
    app.window().with_winit_window(|window| {
        window.request_redraw();
    });
    app.window().request_redraw();
}

fn next_lifecycle_generation() -> u64 {
    WINDOW_LIFECYCLE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

fn current_lifecycle_generation() -> u64 {
    WINDOW_LIFECYCLE_GENERATION.load(Ordering::Acquire)
}

fn native_window_is_visible(app: &MainWindow) -> bool {
    #[cfg(windows)]
    {
        native_window_handle(app).is_some_and(window_is_visible)
    }

    #[cfg(not(windows))]
    {
        app.window().is_visible()
    }
}

fn restore_native_window(app: &MainWindow) {
    #[cfg(windows)]
    if let Some(handle) = native_window_handle(app) {
        restore_window(handle);
    }

    #[cfg(not(windows))]
    let _ = app;
}

fn force_native_window_redraw(app: &MainWindow) {
    #[cfg(windows)]
    if let Some(handle) = native_window_handle(app) {
        force_window_redraw(handle);
    }

    #[cfg(not(windows))]
    let _ = app;
}

#[cfg(windows)]
fn native_window_handle(app: &MainWindow) -> Option<NativeWindowHandle> {
    let mut handle = None;
    app.window().with_winit_window(|window| {
        let Ok(window_handle) = window.window_handle() else {
            return;
        };

        if let RawWindowHandle::Win32(raw) = window_handle.as_raw() {
            handle = NativeWindowHandle::new(raw.hwnd.get());
        }
    });
    handle
}
