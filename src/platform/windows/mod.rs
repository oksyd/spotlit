#![cfg(windows)]

mod desktop_spotlight;
mod desktop_wallpaper;
mod file_dialog;
mod lock_screen;
mod paths;
mod registry;
mod session;
mod shell;
mod single_instance;
mod startup;
mod theme;
mod thread_priority;
mod window;

pub use desktop_spotlight::{current_desktop_spotlight_metadata, desktop_spotlight_creatives};
pub use desktop_wallpaper::current_desktop_wallpaper;
pub use file_dialog::{pick_export_image_path, pick_wallpaper_image};
pub use lock_screen::WindowsLockScreen;
pub use paths::{app_paths, spotlight_source_dirs};
pub use session::lock_workstation;
pub use shell::{open_path, open_url_in_chrome, reveal_path};
pub use single_instance::SingleInstanceGuard;
pub use startup::{StartupRegistration, StartupState};
pub use theme::{SystemTheme, system_theme};
pub use thread_priority::enter_background_thread_mode;
pub use window::{NativeWindowHandle, force_window_redraw, restore_window, window_is_visible};
