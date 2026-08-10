use crate::core::{AppConfig, FavoriteUpdate, SyncReport, Wallpaper};

use crate::{
    command::Command,
    platform::{LockScreenIntegration, SystemTheme},
};

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub current: Option<Wallpaper>,
    pub wallpapers: Vec<Wallpaper>,
    pub config: AppConfig,
    pub system_theme: SystemTheme,
    pub lock_screen_integration: LockScreenIntegration,
}

#[derive(Debug, Clone)]
pub struct SettingsSnapshot {
    pub config: AppConfig,
    pub system_theme: SystemTheme,
    pub lock_screen_integration: LockScreenIntegration,
}

#[derive(Debug, Clone)]
pub enum WorkerEvent {
    AutoSyncIdle,
    ConfigUpdated(String, SettingsSnapshot),
    OpenedPath(String),
    Snapshot(Snapshot),
    Synced(SyncReport, Snapshot),
    FavoriteUpdated(FavoriteUpdate, Snapshot),
    SettingsUpdated(String, Snapshot),
    Failed(CommandFailure),
}

#[derive(Debug, Clone)]
pub struct CommandFailure {
    pub command: Command,
    pub message: String,
}
