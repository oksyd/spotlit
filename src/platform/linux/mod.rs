#![cfg(target_os = "linux")]

mod bing;
mod desktop_wallpaper;
mod file_dialog;
mod gnome_extension;
mod paths;
mod session;
mod shell;
mod single_instance;
mod startup;
mod theme;

pub use bing::{bing_wallpaper_dir, refresh_bing_wallpapers};
pub use desktop_wallpaper::{GnomeLockScreen, current_desktop_wallpaper};
pub use file_dialog::{pick_export_image_path, pick_wallpaper_image};
pub use gnome_extension::{
    install_lock_screen_integration, lock_screen_integration, set_lock_screen_blur_mode,
    set_lock_screen_display_mode, set_lock_screen_integration_enabled,
};
pub use paths::{app_paths, wallpaper_source_dirs};
pub use session::lock_workstation;
pub use shell::{open_path, open_url_in_chrome, reveal_path};
pub use single_instance::SingleInstanceGuard;
pub use startup::{StartupRegistration, StartupState};
pub use theme::{SystemTheme, system_theme};
