use std::{
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

use crate::core::{
    AppConfig, DesktopSpotlightCreative, LanguageMode, SchedulerDecision, SchedulerSettings,
    SpotlightMetadata, SpotlitCore, SpotlitError, ThemeMode, Wallpaper, WallpaperId,
    WallpaperSource, normalized_history_limit,
};

use crate::{
    command::Command,
    diagnostics::{self, Metric},
    platform::{
        LockScreenBlurMode, LockScreenDisplayMode, LockScreenIntegration, LockScreenService,
        PlatformServices, SystemTheme,
    },
};

pub use crate::worker_event::{CommandFailure, SettingsSnapshot, Snapshot, WorkerEvent};
pub use crate::worker_runtime::WorkerHandle;

type WorkerResult<T> = Result<T, WorkerError>;
const THUMBNAIL_WARM_BATCH_SIZE: usize = 8;

#[derive(Debug, Clone, Eq, PartialEq)]
enum WorkerError {
    CoreLockPoisoned,
    Operation(String),
    UnknownLockScreenBlurMode(String),
    UnknownLockScreenDisplayMode(String),
    UnknownLanguage(String),
    UnknownWallpaperSource(String),
    UnknownTheme(String),
}

impl WorkerError {
    fn operation(error: impl fmt::Display) -> Self {
        Self::Operation(error.to_string())
    }

    fn message(message: impl Into<String>) -> Self {
        Self::Operation(message.into())
    }
}

impl fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoreLockPoisoned => formatter.write_str("core worker lock was poisoned"),
            Self::Operation(message) => formatter.write_str(message),
            Self::UnknownLockScreenBlurMode(mode) => {
                write!(formatter, "unknown lock screen blur mode: {mode}")
            }
            Self::UnknownLockScreenDisplayMode(mode) => {
                write!(formatter, "unknown lock screen display mode: {mode}")
            }
            Self::UnknownLanguage(language) => {
                write!(formatter, "unknown interface language: {language}")
            }
            Self::UnknownWallpaperSource(source) => {
                write!(formatter, "unknown wallpaper source: {source}")
            }
            Self::UnknownTheme(theme) => write!(formatter, "unknown theme mode: {theme}"),
        }
    }
}

impl From<SpotlitError> for WorkerError {
    fn from(error: SpotlitError) -> Self {
        Self::operation(error)
    }
}

#[derive(Clone)]
pub struct Worker {
    core: WorkerCore,
    lock_screen: Arc<dyn LockScreenService>,
    platform: Arc<dyn PlatformServices>,
}

#[derive(Clone)]
enum WorkerCore {
    #[cfg(test)]
    Eager(Arc<Mutex<SpotlitCore>>),
    Lazy(Arc<Mutex<LazyCore>>),
}

struct LazyCore {
    paths: crate::core::AppPaths,
    sources: Vec<PathBuf>,
    core: Option<SpotlitCore>,
}

impl WorkerCore {
    fn with_mut<T>(
        &self,
        action: impl FnOnce(&mut SpotlitCore) -> WorkerResult<T>,
    ) -> WorkerResult<T> {
        match self {
            #[cfg(test)]
            Self::Eager(core) => {
                let mut core = core.lock().map_err(|_| WorkerError::CoreLockPoisoned)?;
                action(&mut core)
            }
            Self::Lazy(core) => {
                let mut core = core.lock().map_err(|_| WorkerError::CoreLockPoisoned)?;
                action(core.open()?)
            }
        }
    }

    fn with_ref<T>(&self, action: impl FnOnce(&SpotlitCore) -> WorkerResult<T>) -> WorkerResult<T> {
        match self {
            #[cfg(test)]
            Self::Eager(core) => {
                let core = core.lock().map_err(|_| WorkerError::CoreLockPoisoned)?;
                action(&core)
            }
            Self::Lazy(core) => {
                let mut core = core.lock().map_err(|_| WorkerError::CoreLockPoisoned)?;
                action(core.open()?)
            }
        }
    }
}

impl LazyCore {
    fn open(&mut self) -> WorkerResult<&mut SpotlitCore> {
        if self.core.is_none() {
            let started_at = Instant::now();
            let core = SpotlitCore::open(self.paths.clone(), self.sources.clone())
                .map_err(WorkerError::operation)?;
            tracing::info!(
                elapsed_ms = started_at.elapsed().as_millis(),
                "spotlit core opened"
            );
            self.core = Some(core);
        }

        self.core.as_mut().ok_or(WorkerError::CoreLockPoisoned)
    }
}

impl Worker {
    #[cfg(test)]
    pub fn new(
        core: Arc<Mutex<SpotlitCore>>,
        lock_screen: Arc<dyn LockScreenService>,
        platform: Arc<dyn PlatformServices>,
    ) -> Self {
        Self {
            core: WorkerCore::Eager(core),
            lock_screen,
            platform,
        }
    }

    pub fn open_lazy(
        paths: crate::core::AppPaths,
        sources: Vec<PathBuf>,
        lock_screen: Arc<dyn LockScreenService>,
        platform: Arc<dyn PlatformServices>,
    ) -> Self {
        Self {
            core: WorkerCore::Lazy(Arc::new(Mutex::new(LazyCore {
                paths,
                sources,
                core: None,
            }))),
            lock_screen,
            platform,
        }
    }

    pub fn handle(&self, command: Command) -> WorkerEvent {
        match self.try_handle(command.clone()) {
            Ok(event) => event,
            Err(error) => WorkerEvent::Failed(CommandFailure {
                command,
                message: error.to_string(),
            }),
        }
    }

    fn try_handle(&self, command: Command) -> WorkerResult<WorkerEvent> {
        match command {
            Command::AutoSyncTick => self.handle_auto_sync_tick(),
            Command::CleanCache => self.handle_clean_cache(),
            Command::ExportWallpaper { id } => self.handle_export_wallpaper(WallpaperId::new(id)),
            Command::ImportImage => self.handle_import_image(),
            Command::InstallLockScreenIntegration => self.handle_install_lock_screen_integration(),
            Command::LoadSnapshot => self.handle_load_snapshot(),
            Command::OpenDataFolder => self.handle_open_folder(FolderKind::Data),
            Command::OpenFavoritesFolder => self.handle_open_folder(FolderKind::Favorites),
            Command::OpenLogsFolder => self.handle_open_folder(FolderKind::Logs),
            Command::OpenReleasePage => self.handle_open_release_page(),
            Command::OpenWallpaperInfo { id } => {
                self.handle_open_wallpaper_info(WallpaperId::new(id))
            }
            Command::RevealCurrentImage => self.handle_reveal_current(),
            Command::RevealWallpaper { id } => self.handle_reveal_wallpaper(WallpaperId::new(id)),
            Command::RemoveWallpaper { id } => self.handle_remove_wallpaper(WallpaperId::new(id)),
            Command::Scan => self.handle_scan(),
            Command::SyncCurrent => self.handle_sync_current(),
            Command::SyncWallpaper { id } => self.handle_sync_wallpaper(WallpaperId::new(id)),
            Command::WarmThumbnails => self.handle_warm_thumbnails(),
            Command::SetAutoSync(enabled) => self.handle_set_auto_sync(enabled),
            Command::SetAutomaticUpdateChecks(enabled) => self.update_config(
                |config| config.automatic_update_checks = enabled,
                "Update setting saved",
            ),
            Command::SetFavorite { id, favorite } => {
                self.handle_set_favorite(WallpaperId::new(id), favorite)
            }
            Command::SetHistoryLimit(limit) => self.handle_set_history_limit(limit),
            Command::SetKeepRunningInBackground(enabled) => self.update_config(
                |config| config.keep_running_in_background = enabled,
                "Background setting saved",
            ),
            Command::SetLanguage(language) => {
                let language = parse_language_mode(&language)?;
                self.update_config(
                    |config| config.language = language,
                    "Language setting saved",
                )
            }
            Command::SetLockScreenBlurMode(mode) => {
                self.handle_set_lock_screen_blur_mode(parse_lock_screen_blur_mode(&mode)?)
            }
            Command::SetLockScreenDisplayMode(mode) => {
                self.handle_set_lock_screen_display_mode(parse_lock_screen_display_mode(&mode)?)
            }
            Command::SetLockScreenIntegrationEnabled(enabled) => {
                self.handle_set_lock_screen_integration_enabled(enabled)
            }
            Command::SetWallpaperSource(source) => {
                let source = parse_wallpaper_source(&source)?;
                self.update_config(
                    |config| config.wallpaper_source = source,
                    "Wallpaper source saved",
                )
            }
            Command::SetStartAtLogin(enabled) => self.handle_set_start_at_login(enabled),
            Command::SetSyncInterval(minutes) => self.update_config(
                |config| config.sync_interval_minutes = minutes.clamp(1, 1440),
                "Sync interval saved",
            ),
            Command::SetTheme(theme) => {
                let theme = parse_theme_mode(&theme)?;
                self.update_config(|config| config.theme = theme, "Theme setting saved")
            }
        }
    }

    fn handle_clean_cache(&self) -> WorkerResult<WorkerEvent> {
        self.with_core(|core| {
            let report = core
                .trim_unretained_cache()
                .map_err(WorkerError::operation)?;
            let snapshot = self.snapshot(core);
            Ok(WorkerEvent::SettingsUpdated(
                format!(
                    "Cache cleaned: {} wallpapers removed",
                    report.removed_unretained_wallpapers
                ),
                snapshot,
            ))
        })
    }

    fn handle_install_lock_screen_integration(&self) -> WorkerResult<WorkerEvent> {
        let integration = self
            .platform
            .install_lock_screen_integration()
            .map_err(WorkerError::operation)?;
        self.integration_updated("GNOME extension installed", integration, false)
    }

    fn handle_set_lock_screen_integration_enabled(
        &self,
        enabled: bool,
    ) -> WorkerResult<WorkerEvent> {
        let integration = self
            .platform
            .set_lock_screen_integration_enabled(enabled)
            .map_err(WorkerError::operation)?;
        self.integration_updated(
            if enabled {
                "GNOME extension enabled"
            } else {
                "GNOME extension disabled"
            },
            integration,
            !enabled,
        )
    }

    fn handle_set_lock_screen_blur_mode(
        &self,
        mode: LockScreenBlurMode,
    ) -> WorkerResult<WorkerEvent> {
        let integration = self
            .platform
            .set_lock_screen_blur_mode(mode)
            .map_err(WorkerError::operation)?;
        self.integration_updated("Lock screen blur saved", integration, false)
    }

    fn handle_set_lock_screen_display_mode(
        &self,
        mode: LockScreenDisplayMode,
    ) -> WorkerResult<WorkerEvent> {
        let integration = self
            .platform
            .set_lock_screen_display_mode(mode)
            .map_err(WorkerError::operation)?;
        self.integration_updated("Lock screen display saved", integration, false)
    }

    fn integration_updated(
        &self,
        message: &'static str,
        integration: LockScreenIntegration,
        disable_auto_sync: bool,
    ) -> WorkerResult<WorkerEvent> {
        self.with_core(|core| {
            if disable_auto_sync && core.config().auto_sync_lock_screen {
                let mut config = core.config().clone();
                config.auto_sync_lock_screen = false;
                core.update_config(config).map_err(WorkerError::operation)?;
            }

            let mut settings = self.settings_snapshot(core);
            settings.lock_screen_integration = integration;
            Ok(WorkerEvent::ConfigUpdated(message.to_string(), settings))
        })
    }

    fn handle_export_wallpaper(&self, id: WallpaperId) -> WorkerResult<WorkerEvent> {
        let default_name = self.inspect_core(|core| {
            let wallpaper = core
                .wallpaper(&id)
                .ok_or_else(|| WorkerError::message(format!("wallpaper {id} was not found")))?;
            Ok(export_file_name(&wallpaper))
        })?;

        let Some(target_path) = self
            .platform
            .pick_export_image_path(&default_name)
            .map_err(WorkerError::operation)?
        else {
            return Ok(WorkerEvent::OpenedPath("Export canceled".to_string()));
        };

        let exported_path = self.inspect_core(|core| {
            core.export_wallpaper(&id, &target_path)
                .map_err(WorkerError::operation)
        })?;

        Ok(WorkerEvent::OpenedPath(format!(
            "Exported to {}",
            exported_path.display()
        )))
    }

    fn handle_import_image(&self) -> WorkerResult<WorkerEvent> {
        let Some(path) = self
            .platform
            .pick_wallpaper_image()
            .map_err(WorkerError::operation)?
        else {
            return Ok(WorkerEvent::OpenedPath("Import canceled".to_string()));
        };

        self.with_core(|core| {
            let imported = core
                .import_wallpaper_file_deferred_thumbnail(&path)
                .map_err(WorkerError::operation)?;
            let snapshot = self.snapshot(core);

            if let Some(wallpaper) = imported {
                Ok(WorkerEvent::SettingsUpdated(
                    format!("Imported {}", wallpaper.id),
                    snapshot,
                ))
            } else {
                Ok(WorkerEvent::SettingsUpdated(
                    "Selected file is not a supported landscape wallpaper".to_string(),
                    snapshot,
                ))
            }
        })
    }

    fn handle_load_snapshot(&self) -> WorkerResult<WorkerEvent> {
        self.inspect_core(|core| Ok(WorkerEvent::Snapshot(self.snapshot(core))))
    }

    fn handle_open_folder(&self, folder: FolderKind) -> WorkerResult<WorkerEvent> {
        let path = self.inspect_core(|core| {
            Ok(match folder {
                FolderKind::Data => core.paths().data_dir.clone(),
                FolderKind::Favorites => core.paths().favorite_dir.clone(),
                FolderKind::Logs => core.paths().log_dir.clone(),
            })
        })?;

        self.platform
            .open_path(&path)
            .map_err(WorkerError::operation)?;
        Ok(WorkerEvent::OpenedPath(folder.opened_message().to_string()))
    }

    fn handle_open_release_page(&self) -> WorkerResult<WorkerEvent> {
        self.platform
            .open_url_in_chrome(crate::update::RELEASES_URL)
            .map_err(WorkerError::operation)?;
        Ok(WorkerEvent::OpenedPath(
            "Opened Spotlit release page".to_string(),
        ))
    }

    fn handle_reveal_current(&self) -> WorkerResult<WorkerEvent> {
        let image_path = self.inspect_core(|core| {
            let current = core
                .current_wallpaper()
                .ok_or_else(|| WorkerError::message("no wallpaper has been discovered yet"))?;
            Ok(current.best_image_path().to_path_buf())
        })?;

        self.platform
            .reveal_path(&image_path)
            .map_err(WorkerError::operation)?;
        Ok(WorkerEvent::OpenedPath("Opened current image".to_string()))
    }

    fn handle_open_wallpaper_info(&self, id: WallpaperId) -> WorkerResult<WorkerEvent> {
        let info_url = self.inspect_core(|core| {
            let wallpaper = core
                .wallpaper(&id)
                .ok_or_else(|| WorkerError::message(format!("wallpaper {id} was not found")))?;
            wallpaper
                .spotlight
                .info_url
                .clone()
                .ok_or_else(|| WorkerError::message(format!("wallpaper {id} has no info URL")))
        })?;

        self.platform
            .open_url_in_chrome(&info_url)
            .map_err(WorkerError::operation)?;
        Ok(WorkerEvent::OpenedPath("Opened wallpaper info".to_string()))
    }

    fn handle_reveal_wallpaper(&self, id: WallpaperId) -> WorkerResult<WorkerEvent> {
        let image_path = self.inspect_core(|core| {
            let wallpaper = core
                .wallpaper(&id)
                .ok_or_else(|| WorkerError::message(format!("wallpaper {id} was not found")))?;
            Ok(wallpaper.best_image_path().to_path_buf())
        })?;

        self.platform
            .reveal_path(&image_path)
            .map_err(WorkerError::operation)?;
        Ok(WorkerEvent::OpenedPath(format!("Opened {id}")))
    }

    fn handle_remove_wallpaper(&self, id: WallpaperId) -> WorkerResult<WorkerEvent> {
        self.with_core(|core| {
            core.remove_wallpaper(&id).map_err(WorkerError::operation)?;
            let snapshot = self.snapshot(core);
            Ok(WorkerEvent::SettingsUpdated(
                format!("Removed {id}"),
                snapshot,
            ))
        })
    }

    fn handle_scan(&self) -> WorkerResult<WorkerEvent> {
        self.with_core(|core| {
            let report = core
                .scan_spotlight_wallpapers_deferred_thumbnails()
                .map_err(WorkerError::operation)?;
            let desktop_spotlight_creatives = self.desktop_spotlight_creatives();

            self.import_desktop_spotlight_creatives(core, &desktop_spotlight_creatives);

            if let Err(error) =
                self.import_current_desktop_wallpaper(core, &desktop_spotlight_creatives)
            {
                tracing::warn!(
                    error = %error,
                    "failed to import current desktop wallpaper during refresh"
                );
            }

            let maintenance = core
                .maintain_library_lightweight()
                .map_err(WorkerError::operation)?;
            if maintenance.has_changes() {
                tracing::info!(
                    removed_missing_wallpapers = maintenance.removed_missing_wallpapers,
                    cleared_missing_thumbnails = maintenance.cleared_missing_thumbnails,
                    cleared_missing_favorites = maintenance.cleared_missing_favorites,
                    removed_unretained_wallpapers = maintenance.removed_unretained_wallpapers,
                    normalized_wallpapers = maintenance.normalized_wallpapers,
                    regenerated_thumbnails = maintenance.regenerated_thumbnails,
                    "library maintenance finished"
                );
            }

            let snapshot = self.snapshot(core);
            tracing::info!(
                inserted = report.inserted,
                updated = report.updated,
                "spotlight scan finished"
            );
            Ok(WorkerEvent::Snapshot(snapshot))
        })
    }

    fn handle_warm_thumbnails(&self) -> WorkerResult<WorkerEvent> {
        self.with_core(|core| {
            let started_at = Instant::now();
            let maintenance = core
                .warm_thumbnail_cache(THUMBNAIL_WARM_BATCH_SIZE)
                .map_err(WorkerError::operation)?;
            diagnostics::record(Metric::Thumbnails, started_at.elapsed());

            if maintenance.has_changes() {
                tracing::info!(
                    removed_missing_wallpapers = maintenance.removed_missing_wallpapers,
                    cleared_missing_thumbnails = maintenance.cleared_missing_thumbnails,
                    cleared_missing_favorites = maintenance.cleared_missing_favorites,
                    removed_unretained_wallpapers = maintenance.removed_unretained_wallpapers,
                    normalized_wallpapers = maintenance.normalized_wallpapers,
                    regenerated_thumbnails = maintenance.regenerated_thumbnails,
                    "thumbnail warm-up finished"
                );

                Ok(WorkerEvent::SettingsUpdated(
                    "Preview cache updated".to_string(),
                    self.snapshot(core),
                ))
            } else {
                Ok(WorkerEvent::AutoSyncIdle)
            }
        })
    }

    fn handle_sync_current(&self) -> WorkerResult<WorkerEvent> {
        let prepared = self.with_core(|core| self.prepare_current_desktop_sync(core))?;

        self.apply_prepared_sync(prepared)
    }

    fn handle_sync_wallpaper(&self, id: WallpaperId) -> WorkerResult<WorkerEvent> {
        let prepared = self.inspect_core(|core| {
            let wallpaper = core
                .wallpaper_for_sync_by_id(&id)
                .map_err(WorkerError::operation)?;
            prepare_wallpaper_sync(core, wallpaper)
        })?;

        self.apply_prepared_sync(prepared)
    }

    fn handle_set_favorite(&self, id: WallpaperId, favorite: bool) -> WorkerResult<WorkerEvent> {
        self.with_core(|core| {
            let update = core
                .set_favorite(&id, favorite)
                .map_err(WorkerError::operation)?;
            core.enforce_history_limit()
                .map_err(WorkerError::operation)?;
            let snapshot = self.snapshot(core);
            Ok(WorkerEvent::FavoriteUpdated(update, snapshot))
        })
    }

    fn handle_set_history_limit(&self, limit: Option<u16>) -> WorkerResult<WorkerEvent> {
        self.with_core(|core| {
            let mut config = core.config().clone();
            config.max_history_wallpapers = limit.map(normalized_history_limit);
            core.update_config(config).map_err(WorkerError::operation)?;
            let maintenance = core
                .enforce_history_limit()
                .map_err(WorkerError::operation)?;
            Ok(WorkerEvent::SettingsUpdated(
                history_limit_message(maintenance.removed_unretained_wallpapers),
                self.snapshot(core),
            ))
        })
    }

    fn handle_set_start_at_login(&self, enabled: bool) -> WorkerResult<WorkerEvent> {
        let startup_state = self
            .platform
            .set_startup_enabled(enabled)
            .map_err(WorkerError::operation)?;

        self.update_config(
            |config| config.start_at_login = startup_state.is_enabled(),
            "Startup setting saved",
        )
    }

    fn handle_set_auto_sync(&self, enabled: bool) -> WorkerResult<WorkerEvent> {
        let was_enabled = self.inspect_core(|core| Ok(core.config().auto_sync_lock_screen))?;
        if !enabled || was_enabled {
            return self.update_config(
                |config| config.auto_sync_lock_screen = enabled,
                "Settings saved",
            );
        }

        let prepared = self.with_core(|core| self.prepare_immediate_auto_sync(core))?;
        self.set_lock_screen_wallpaper(&prepared)?;

        self.with_core(|core| {
            core.record_lock_screen_sync(&prepared.id)
                .map_err(WorkerError::operation)?;
            let mut config = core.config().clone();
            config.auto_sync_lock_screen = true;
            core.update_config(config).map_err(WorkerError::operation)?;
            Ok(WorkerEvent::SettingsUpdated(
                "Auto sync enabled and synced".to_string(),
                self.snapshot(core),
            ))
        })
    }

    fn handle_auto_sync_tick(&self) -> WorkerResult<WorkerEvent> {
        let prepared = self.with_core(|core| {
            let config = core.config().clone();
            if !config.auto_sync_lock_screen {
                return Ok(AutoSyncOutcome::Idle);
            }

            let settings = SchedulerSettings {
                auto_sync_lock_screen: config.auto_sync_lock_screen,
                sync_interval_minutes: config.sync_interval_minutes,
            };

            match config.wallpaper_source {
                WallpaperSource::CurrentDesktop => {
                    self.prepare_current_desktop_auto_sync(core, &settings)
                }
                WallpaperSource::RandomLibrary | WallpaperSource::RandomFavorites => {
                    self.prepare_rotation_auto_sync(core, &settings, config.wallpaper_source)
                }
            }
        })?;

        match prepared {
            AutoSyncOutcome::Idle => Ok(WorkerEvent::AutoSyncIdle),
            AutoSyncOutcome::Sync(prepared) => self.apply_prepared_sync(prepared),
        }
    }

    fn prepare_immediate_auto_sync(&self, core: &mut SpotlitCore) -> WorkerResult<PreparedSync> {
        match core.config().wallpaper_source {
            WallpaperSource::CurrentDesktop => self.prepare_current_desktop_sync(core),
            WallpaperSource::RandomLibrary | WallpaperSource::RandomFavorites => {
                let source = core.config().wallpaper_source;
                let wallpaper = core.wallpaper_rotation_candidate(source).ok_or_else(|| {
                    WorkerError::message(
                        "no wallpaper is available for the selected auto sync source",
                    )
                })?;
                prepare_wallpaper_sync(core, wallpaper)
            }
        }
    }

    fn prepare_current_desktop_sync(&self, core: &mut SpotlitCore) -> WorkerResult<PreparedSync> {
        let desktop_spotlight_creatives = self.desktop_spotlight_creatives();
        self.import_desktop_spotlight_creatives(core, &desktop_spotlight_creatives);
        let (preferred_source, imported_current) =
            self.import_current_desktop_wallpaper(core, &desktop_spotlight_creatives)?;
        let wallpaper = match imported_current {
            Some(wallpaper) => wallpaper,
            None => core
                .wallpaper_for_sync(preferred_source.as_deref())
                .map_err(WorkerError::operation)?,
        };
        prepare_wallpaper_sync(core, wallpaper)
    }

    fn prepare_current_desktop_auto_sync(
        &self,
        core: &mut SpotlitCore,
        settings: &SchedulerSettings,
    ) -> WorkerResult<AutoSyncOutcome> {
        let desktop_path = self
            .platform
            .current_desktop_wallpaper()
            .map_err(WorkerError::operation)?;
        let desktop_spotlight_creatives = self.desktop_spotlight_creatives();
        self.import_desktop_spotlight_creatives(core, &desktop_spotlight_creatives);

        if let Some(current) = core.current_wallpaper() {
            let desktop_matches = desktop_path
                .as_deref()
                .is_some_and(|path| same_path(path, &current.source_path));

            if desktop_matches || desktop_path.is_none() {
                let current = match desktop_path.as_deref() {
                    Some(path) => self.attach_desktop_spotlight_metadata(
                        core,
                        current,
                        path,
                        &desktop_spotlight_creatives,
                    )?,
                    None => current,
                };

                return Ok(match settings.decide(current.last_synced_at) {
                    SchedulerDecision::SyncNow => {
                        AutoSyncOutcome::Sync(prepare_wallpaper_sync(core, current)?)
                    }
                    SchedulerDecision::Wait | SchedulerDecision::Disabled => AutoSyncOutcome::Idle,
                });
            }
        }

        let imported_current = match desktop_path {
            Some(path) if path.exists() => {
                self.import_desktop_wallpaper_path(core, &path, &desktop_spotlight_creatives)?
            }
            Some(path) => {
                tracing::debug!(
                    path = %path.display(),
                    "current desktop wallpaper path does not exist"
                );
                None
            }
            None => None,
        };
        let Some(current) = imported_current.or_else(|| core.current_wallpaper()) else {
            return Ok(AutoSyncOutcome::Idle);
        };

        match settings.decide(current.last_synced_at) {
            SchedulerDecision::SyncNow => Ok(AutoSyncOutcome::Sync(prepare_wallpaper_sync(
                core, current,
            )?)),
            SchedulerDecision::Wait | SchedulerDecision::Disabled => Ok(AutoSyncOutcome::Idle),
        }
    }

    fn prepare_rotation_auto_sync(
        &self,
        core: &mut SpotlitCore,
        settings: &SchedulerSettings,
        source: WallpaperSource,
    ) -> WorkerResult<AutoSyncOutcome> {
        if !matches!(
            settings.decide(core.latest_wallpaper_rotation_sync_at(source)),
            SchedulerDecision::SyncNow
        ) {
            return Ok(AutoSyncOutcome::Idle);
        }

        let Some(candidate) = core.wallpaper_rotation_candidate(source) else {
            tracing::debug!(?source, "auto sync source has no wallpaper candidate");
            return Ok(AutoSyncOutcome::Idle);
        };

        Ok(AutoSyncOutcome::Sync(prepare_wallpaper_sync(
            core, candidate,
        )?))
    }

    fn import_current_desktop_wallpaper(
        &self,
        core: &mut SpotlitCore,
        desktop_spotlight_creatives: &[DesktopSpotlightCreative],
    ) -> WorkerResult<(Option<PathBuf>, Option<Wallpaper>)> {
        let Some(path) = self
            .platform
            .current_desktop_wallpaper()
            .map_err(WorkerError::operation)?
        else {
            return Ok((None, None));
        };

        if !path.exists() {
            tracing::debug!(
                path = %path.display(),
                "current desktop wallpaper path does not exist"
            );
            return Ok((Some(path), None));
        }

        let wallpaper =
            self.import_desktop_wallpaper_path(core, &path, desktop_spotlight_creatives)?;
        Ok((Some(path), wallpaper))
    }

    fn import_desktop_wallpaper_path(
        &self,
        core: &mut SpotlitCore,
        path: &Path,
        desktop_spotlight_creatives: &[DesktopSpotlightCreative],
    ) -> WorkerResult<Option<Wallpaper>> {
        let wallpaper = core
            .import_wallpaper_file_deferred_thumbnail(path)
            .map_err(WorkerError::operation)?;

        match wallpaper {
            Some(wallpaper) => Ok(Some(self.attach_desktop_spotlight_metadata(
                core,
                wallpaper,
                path,
                desktop_spotlight_creatives,
            )?)),
            None => Ok(None),
        }
    }

    fn import_desktop_spotlight_creatives(
        &self,
        core: &mut SpotlitCore,
        desktop_spotlight_creatives: &[DesktopSpotlightCreative],
    ) -> usize {
        let mut imported = 0;
        for creative in desktop_spotlight_creatives {
            if !creative.landscape_path.exists() {
                tracing::debug!(
                    path = %creative.landscape_path.display(),
                    "desktop Spotlight creative image path does not exist"
                );
                continue;
            }

            let wallpaper =
                match core.import_wallpaper_file_deferred_thumbnail(&creative.landscape_path) {
                    Ok(Some(wallpaper)) => wallpaper,
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(
                            path = %creative.landscape_path.display(),
                            error = %error,
                            "failed to import desktop Spotlight creative image"
                        );
                        continue;
                    }
                };

            if let Err(error) = self.backfill_wallpaper_spotlight_metadata(
                core,
                wallpaper,
                creative.metadata.clone(),
            ) {
                tracing::warn!(
                    path = %creative.landscape_path.display(),
                    error = %error,
                    "failed to backfill desktop Spotlight creative metadata"
                );
                continue;
            }
            imported += 1;
        }

        if imported > 0 {
            tracing::info!(imported, "desktop Spotlight creative batch imported");
        }

        imported
    }

    fn desktop_spotlight_creatives(&self) -> Vec<DesktopSpotlightCreative> {
        match self.platform.desktop_spotlight_creatives() {
            Ok(creatives) => creatives,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to read desktop Spotlight creative batch metadata"
                );
                Vec::new()
            }
        }
    }

    fn attach_desktop_spotlight_metadata(
        &self,
        core: &mut SpotlitCore,
        wallpaper: Wallpaper,
        source_path: &Path,
        desktop_spotlight_creatives: &[DesktopSpotlightCreative],
    ) -> WorkerResult<Wallpaper> {
        if let Some(metadata) =
            metadata_for_desktop_spotlight_path(source_path, desktop_spotlight_creatives)
        {
            return self.backfill_wallpaper_spotlight_metadata(core, wallpaper, metadata);
        }

        self.attach_current_desktop_spotlight_metadata(core, wallpaper)
    }

    fn attach_current_desktop_spotlight_metadata(
        &self,
        core: &mut SpotlitCore,
        wallpaper: Wallpaper,
    ) -> WorkerResult<Wallpaper> {
        let metadata = match self.platform.current_desktop_spotlight_metadata() {
            Ok(Some(metadata)) => metadata,
            Ok(None) => return Ok(wallpaper),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "failed to read current desktop spotlight metadata"
                );
                return Ok(wallpaper);
            }
        };

        self.backfill_wallpaper_spotlight_metadata(core, wallpaper, metadata)
    }

    fn backfill_wallpaper_spotlight_metadata(
        &self,
        core: &mut SpotlitCore,
        wallpaper: Wallpaper,
        metadata: SpotlightMetadata,
    ) -> WorkerResult<Wallpaper> {
        if metadata.is_empty() {
            return Ok(wallpaper);
        }

        core.backfill_wallpaper_spotlight_metadata(&wallpaper.id, metadata)
            .map(|updated| updated.unwrap_or(wallpaper))
            .map_err(WorkerError::operation)
    }

    fn update_config(
        &self,
        update: impl FnOnce(&mut AppConfig),
        message: &'static str,
    ) -> WorkerResult<WorkerEvent> {
        self.with_core(|core| {
            let mut config = core.config().clone();
            update(&mut config);
            core.update_config(config).map_err(WorkerError::operation)?;
            Ok(WorkerEvent::ConfigUpdated(
                message.to_string(),
                self.settings_snapshot(core),
            ))
        })
    }

    fn settings_snapshot(&self, core: &SpotlitCore) -> SettingsSnapshot {
        let mut config = core.config().clone();
        if let Ok(startup_state) = self.platform.startup_state() {
            config.start_at_login = startup_state.is_enabled();
        }

        let system_theme = self.platform.system_theme().unwrap_or(SystemTheme::Light);
        let lock_screen_integration =
            self.platform
                .lock_screen_integration()
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "failed to query lock screen integration");
                    LockScreenIntegration::default()
                });

        SettingsSnapshot {
            config,
            system_theme,
            lock_screen_integration,
        }
    }

    fn snapshot(&self, core: &SpotlitCore) -> Snapshot {
        let settings = self.settings_snapshot(core);

        Snapshot {
            current: core.current_wallpaper(),
            wallpapers: core.list_wallpapers(),
            config: settings.config,
            system_theme: settings.system_theme,
            lock_screen_integration: settings.lock_screen_integration,
        }
    }

    fn with_core<T>(
        &self,
        action: impl FnOnce(&mut SpotlitCore) -> WorkerResult<T>,
    ) -> WorkerResult<T> {
        self.core.with_mut(action)
    }

    fn inspect_core<T>(
        &self,
        action: impl FnOnce(&SpotlitCore) -> WorkerResult<T>,
    ) -> WorkerResult<T> {
        self.core.with_ref(action)
    }

    fn apply_prepared_sync(&self, prepared: PreparedSync) -> WorkerResult<WorkerEvent> {
        self.set_lock_screen_wallpaper(&prepared)?;

        self.with_core(|core| {
            let report = core
                .record_lock_screen_sync(&prepared.id)
                .map_err(WorkerError::operation)?;
            let snapshot = self.snapshot(core);
            Ok(WorkerEvent::Synced(report, snapshot))
        })
    }

    fn set_lock_screen_wallpaper(&self, prepared: &PreparedSync) -> WorkerResult<()> {
        let diagnostics = SyncImageDiagnostics::inspect(&prepared.lock_screen_image_path);
        tracing::info!(
            wallpaper_id = %prepared.id,
            wallpaper_title = %prepared.title,
            spotlight_id = ?prepared.spotlight_id.as_deref(),
            library_image_path = %prepared.library_image_path.display(),
            lock_screen_image_path = %prepared.lock_screen_image_path.display(),
            image_extension = %diagnostics.extension,
            image_exists = diagnostics.exists,
            image_is_file = diagnostics.is_file,
            image_bytes = ?diagnostics.bytes,
            image_width = prepared.width,
            image_height = prepared.height,
            "lock screen sync started"
        );

        if let Err(error) = self
            .lock_screen
            .set_lock_screen_wallpaper(&prepared.lock_screen_image_path)
        {
            tracing::warn!(
                wallpaper_id = %prepared.id,
                wallpaper_title = %prepared.title,
                spotlight_id = ?prepared.spotlight_id.as_deref(),
                library_image_path = %prepared.library_image_path.display(),
                lock_screen_image_path = %prepared.lock_screen_image_path.display(),
                image_extension = %diagnostics.extension,
                image_exists = diagnostics.exists,
                image_is_file = diagnostics.is_file,
                image_bytes = ?diagnostics.bytes,
                image_width = prepared.width,
                image_height = prepared.height,
                error = %error,
                "lock screen sync failed"
            );
            return Err(WorkerError::operation(error));
        }

        tracing::info!(
            wallpaper_id = %prepared.id,
            wallpaper_title = %prepared.title,
            spotlight_id = ?prepared.spotlight_id.as_deref(),
            library_image_path = %prepared.library_image_path.display(),
            lock_screen_image_path = %prepared.lock_screen_image_path.display(),
            "lock screen sync applied"
        );

        Ok(())
    }
}

fn prepare_wallpaper_sync(core: &SpotlitCore, wallpaper: Wallpaper) -> WorkerResult<PreparedSync> {
    let lock_screen_image_path = core
        .prepare_lock_screen_image(&wallpaper)
        .map_err(WorkerError::operation)?;
    Ok(PreparedSync::from_wallpaper(
        wallpaper,
        lock_screen_image_path,
    ))
}

#[derive(Debug, Clone)]
struct PreparedSync {
    id: WallpaperId,
    title: String,
    spotlight_id: Option<String>,
    library_image_path: PathBuf,
    lock_screen_image_path: PathBuf,
    width: u32,
    height: u32,
}

impl PreparedSync {
    fn from_wallpaper(wallpaper: Wallpaper, lock_screen_image_path: PathBuf) -> Self {
        let library_image_path = wallpaper.best_image_path().to_path_buf();
        let title = wallpaper.display_title().to_string();
        Self {
            id: wallpaper.id,
            title,
            spotlight_id: wallpaper.spotlight.spotlight_id,
            library_image_path,
            lock_screen_image_path,
            width: wallpaper.width,
            height: wallpaper.height,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct SyncImageDiagnostics {
    extension: String,
    exists: bool,
    is_file: bool,
    bytes: Option<u64>,
}

impl SyncImageDiagnostics {
    fn inspect(path: &Path) -> Self {
        let metadata = fs::metadata(path).ok();
        let is_file = metadata.as_ref().is_some_and(|metadata| metadata.is_file());

        Self {
            extension: path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase(),
            exists: metadata.is_some(),
            is_file,
            bytes: metadata
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len()),
        }
    }
}

#[derive(Debug, Clone)]
enum AutoSyncOutcome {
    Idle,
    Sync(PreparedSync),
}

#[derive(Debug, Clone, Copy)]
enum FolderKind {
    Data,
    Favorites,
    Logs,
}

impl FolderKind {
    fn opened_message(self) -> &'static str {
        match self {
            FolderKind::Data => "Opened data folder",
            FolderKind::Favorites => "Opened favorites folder",
            FolderKind::Logs => "Opened logs folder",
        }
    }
}

fn parse_theme_mode(value: &str) -> WorkerResult<ThemeMode> {
    match value {
        "light" => Ok(ThemeMode::Light),
        "dark" => Ok(ThemeMode::Dark),
        "system" => Ok(ThemeMode::System),
        other => Err(WorkerError::UnknownTheme(other.to_string())),
    }
}

fn parse_language_mode(value: &str) -> WorkerResult<LanguageMode> {
    match value {
        "system" => Ok(LanguageMode::System),
        "en" => Ok(LanguageMode::English),
        "zh-CN" | "zh_cn" => Ok(LanguageMode::SimplifiedChinese),
        "de" => Ok(LanguageMode::German),
        other => Err(WorkerError::UnknownLanguage(other.to_string())),
    }
}

fn parse_lock_screen_blur_mode(value: &str) -> WorkerResult<LockScreenBlurMode> {
    match value {
        "system" => Ok(LockScreenBlurMode::System),
        "soft" => Ok(LockScreenBlurMode::Soft),
        "clear" => Ok(LockScreenBlurMode::Clear),
        other => Err(WorkerError::UnknownLockScreenBlurMode(other.to_string())),
    }
}

fn parse_lock_screen_display_mode(value: &str) -> WorkerResult<LockScreenDisplayMode> {
    match value {
        "system" => Ok(LockScreenDisplayMode::System),
        "keep-on-ac" => Ok(LockScreenDisplayMode::PluggedIn),
        "keep-on" => Ok(LockScreenDisplayMode::Always),
        other => Err(WorkerError::UnknownLockScreenDisplayMode(other.to_string())),
    }
}

fn parse_wallpaper_source(value: &str) -> WorkerResult<WallpaperSource> {
    match value {
        "current_desktop" | "current" => Ok(WallpaperSource::CurrentDesktop),
        "random_library" | "library" => Ok(WallpaperSource::RandomLibrary),
        "random_favorites" | "favorites" => Ok(WallpaperSource::RandomFavorites),
        other => Err(WorkerError::UnknownWallpaperSource(other.to_string())),
    }
}

fn history_limit_message(removed_wallpapers: usize) -> String {
    if removed_wallpapers == 0 {
        return "History limit saved".to_string();
    }

    format!("History limit saved: {removed_wallpapers} wallpapers removed")
}

fn export_file_name(wallpaper: &Wallpaper) -> String {
    let extension = wallpaper
        .best_image_path()
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("jpg");

    format!("{}.{}", wallpaper.file_stem(), extension)
}

fn metadata_for_desktop_spotlight_path(
    path: &Path,
    desktop_spotlight_creatives: &[DesktopSpotlightCreative],
) -> Option<SpotlightMetadata> {
    desktop_spotlight_creatives
        .iter()
        .find(|creative| same_path(&creative.landscape_path, path))
        .map(|creative| creative.metadata.clone())
}

fn same_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}
