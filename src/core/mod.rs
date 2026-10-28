#![allow(dead_code)]

mod config;
#[cfg(test)]
mod core_tests;
mod engine;
mod error;
mod favorite;
mod fs_utils;
mod library;
#[cfg(target_os = "linux")]
mod lock_screen_info;
mod model;
mod retention;
mod scheduler;
mod spotlight;
mod sync;
mod thumbnail;

#[cfg(target_os = "linux")]
pub(crate) use config::write_json_file;
pub use config::{
    AppConfig, ConfigStore, LanguageMode, ThemeMode, UpdateProxy, UpdateProxyMode, WallpaperSource,
    normalized_history_limit,
};
pub use engine::SpotlitCore;
pub use error::{Result, SpotlitError};
pub use favorite::FavoriteUpdate;
pub use library::WallpaperLibrary;
pub use model::{
    AppPaths, AppliedWallpaper, DesktopSpotlightCreative, LibraryMaintenanceReport,
    SpotlightMetadata, Wallpaper, WallpaperId,
};
pub use scheduler::{SchedulerDecision, SchedulerSettings};
pub use spotlight::{ScanReport, SpotlightScanner};
pub use sync::SyncReport;
pub use thumbnail::ensure_thumbnail;
