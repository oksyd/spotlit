use std::{
    io,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::core::{AppConfig, AppPaths};
use slint::{ComponentHandle, Model, ModelRc, SharedString};

use crate::{
    MainWindow, WallpaperItem,
    bridge::PreparedSnapshot,
    command::Command,
    diagnostics::{self, Metric},
    image_cache,
    platform::{LockScreenService, PlatformServices},
    preview_image::{DecodedPreviewImage, decode_display_image},
    ui_events::{
        PreparedWorkerEvent, UiSink, post_prepared_worker_event, prepare_worker_event,
        window_accepts_image_work,
    },
    worker::{SettingsSnapshot, Worker, WorkerHandle},
};

const SCHEDULER_TICK: Duration = Duration::from_secs(60);
const PRESENT_REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const CACHED_SNAPSHOT_APPLY_DELAY: Duration = Duration::from_millis(16);
const SNAPSHOT_LOAD_AFTER_PRESENT_DELAY: Duration = Duration::from_millis(96);
const PRESENT_REFRESH_DELAY: Duration = Duration::from_millis(1500);
const SELECTED_PREVIEW_RESULT_RETRY_DELAY: Duration = Duration::from_millis(32);
const SELECTED_PREVIEW_RESULT_RETRIES: u8 = 8;
const UPDATE_CHECK_DELAY: Duration = Duration::from_millis(700);
const UPDATE_CONFIG_RETRY_DELAY: Duration = Duration::from_millis(150);
const UPDATE_CONFIG_RETRIES: u8 = 60;
const BACKGROUND_THREAD_STACK_SIZE: usize = 256 * 1024;
const IMAGE_THREAD_STACK_SIZE: usize = 512 * 1024;
const EDGE_NAVIGATION_DELTA: i32 = 1_000_000;

#[derive(Clone)]
pub struct AppState {
    worker: WorkerHandle,
    latest_snapshot: Arc<Mutex<Option<Arc<PreparedSnapshot>>>>,
    snapshot_load_pending: Arc<AtomicBool>,
    last_present_refresh: Arc<Mutex<Option<Instant>>>,
    present_snapshot_generation: Arc<AtomicU64>,
    preview_generation: Arc<AtomicU64>,
    preview_loader: PreviewLoaderHandle,
    auto_sync_state: Arc<(Mutex<bool>, Condvar)>,
    update_dir: PathBuf,
    update_operation_pending: Arc<AtomicBool>,
    automatic_update_check_decided: Arc<AtomicBool>,
    available_update: Arc<Mutex<Option<crate::update::ReleaseInfo>>>,
    prepared_update: Arc<Mutex<Option<crate::update::PreparedUpdate>>>,
}

#[derive(Clone)]
struct PreviewLoaderHandle {
    state: Arc<PreviewLoaderState>,
    ui: UiSink,
    generation: Arc<AtomicU64>,
    started: Arc<AtomicBool>,
}

struct PreviewLoaderState {
    pending: Mutex<Option<PreviewLoadRequest>>,
    last_request: Mutex<Option<PreviewRequestKey>>,
    changed: Condvar,
}

#[derive(Clone)]
struct PreviewLoadRequest {
    id: String,
    path: PathBuf,
    path_text: String,
    generation: u64,
}

#[derive(Clone, Eq, PartialEq)]
struct PreviewRequestKey {
    id: String,
    path_text: String,
}

struct ReadySelectedPreview {
    request: PreviewLoadRequest,
    image: DecodedPreviewImage,
}

impl AppState {
    pub fn open_lazy(
        ui: UiSink,
        paths: AppPaths,
        sources: Vec<PathBuf>,
        lock_screen: Arc<dyn LockScreenService>,
        platform: Arc<dyn PlatformServices>,
    ) -> io::Result<Self> {
        let update_dir = paths.data_dir.join("updates");
        let worker = Worker::open_lazy(paths, sources, lock_screen, platform);
        Self::start(ui, worker, false, update_dir)
    }

    fn start(
        ui: UiSink,
        worker: Worker,
        auto_sync_enabled: bool,
        update_dir: PathBuf,
    ) -> io::Result<Self> {
        let latest_snapshot = Arc::new(Mutex::new(None));
        let snapshot_load_pending = Arc::new(AtomicBool::new(false));
        let auto_sync_state = Arc::new((Mutex::new(auto_sync_enabled), Condvar::new()));
        let update_operation_pending = Arc::new(AtomicBool::new(false));
        let automatic_update_check_decided = Arc::new(AtomicBool::new(false));
        let ui_events = ui.clone();
        let worker_latest_snapshot = Arc::clone(&latest_snapshot);
        let worker_snapshot_load_pending = Arc::clone(&snapshot_load_pending);
        let worker_auto_sync_state = Arc::clone(&auto_sync_state);
        let worker = WorkerHandle::deferred(worker, move |event| {
            let event = prepare_worker_event(event);
            let post_decision = worker_event_post_decision(&worker_latest_snapshot, &event);
            cache_latest_snapshot(&worker_latest_snapshot, &event);
            clear_snapshot_load_pending(&worker_snapshot_load_pending, &event);
            update_scheduler_from_event(&worker_auto_sync_state, &event);

            match post_decision {
                WorkerEventPostDecision::Full => {
                    post_prepared_worker_event(ui_events.clone(), event)
                }
                WorkerEventPostDecision::SnapshotUnchanged => post_prepared_worker_event(
                    ui_events.clone(),
                    PreparedWorkerEvent::SnapshotUnchanged,
                ),
            }
        });
        let present_snapshot_generation = Arc::new(AtomicU64::new(0));
        let preview_generation = Arc::new(AtomicU64::new(0));
        let preview_loader = PreviewLoaderHandle::new(ui, Arc::clone(&preview_generation));

        Ok(Self {
            worker,
            latest_snapshot,
            snapshot_load_pending,
            last_present_refresh: Arc::new(Mutex::new(None)),
            present_snapshot_generation,
            preview_generation,
            preview_loader,
            auto_sync_state,
            update_dir,
            update_operation_pending,
            automatic_update_check_decided,
            available_update: Arc::new(Mutex::new(None)),
            prepared_update: Arc::new(Mutex::new(None)),
        })
    }

    pub fn install(&self, app: &MainWindow) {
        self.restore_cached_update_state(app);

        let state = self.clone();
        let ui = app.as_weak();
        app.on_refresh_requested(move || {
            if !state.refresh_async() {
                clear_pending_refresh(&ui);
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_library_filter_selected(move |favorites_only| {
            let Some(app) = ui.upgrade() else {
                return;
            };

            state.select_library_filter(&app, favorites_only);
        });

        let ui = app.as_weak();
        app.on_action_status_requested(move |message| {
            let Some(app) = ui.upgrade() else {
                return;
            };

            crate::ui_events::set_action_feedback_status(&app, message);
        });

        let ui = app.as_weak();
        app.on_action_feedback_cleared(move || {
            let Some(app) = ui.upgrade() else {
                return;
            };

            crate::ui_events::clear_action_feedback(&app);
        });

        let ui = app.as_weak();
        app.on_settings_status_requested(move |message| {
            let Some(app) = ui.upgrade() else {
                return;
            };

            crate::ui_events::set_settings_feedback_status(&app, message);
        });

        let ui = app.as_weak();
        app.on_settings_feedback_cleared(move || {
            let Some(app) = ui.upgrade() else {
                return;
            };

            crate::ui_events::clear_settings_feedback(&app);
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_wallpaper_navigation_requested(move |delta| {
            let Some(app) = ui.upgrade() else {
                return;
            };

            state.navigate_wallpaper(&app, delta);
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_sync_requested(move || {
            if !state.dispatch(Command::SyncCurrent) {
                clear_pending_sync(&ui);
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_clean_cache_requested(move || {
            if !state.dispatch(Command::CleanCache) {
                clear_pending_clean_cache(&ui);
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_import_image_requested(move || {
            if !state.dispatch(Command::ImportImage) {
                clear_pending_import(&ui);
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_auto_sync_changed(move |enabled| {
            if !state.dispatch(Command::SetAutoSync(enabled)) {
                clear_pending_settings_save(&ui, "Settings request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_gnome_integration_install_requested(move || {
            if !state.dispatch(Command::InstallLockScreenIntegration) {
                clear_pending_settings_save(&ui, "Extension install request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_gnome_integration_enabled_changed(move |enabled| {
            if !state.dispatch(Command::SetLockScreenIntegrationEnabled(enabled)) {
                clear_pending_settings_save(&ui, "Extension request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_lock_screen_blur_selected(move |mode| {
            if !state.dispatch(Command::SetLockScreenBlurMode(mode.to_string())) {
                clear_pending_settings_save(&ui, "Blur setting request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_lock_screen_display_selected(move |mode| {
            if !state.dispatch(Command::SetLockScreenDisplayMode(mode.to_string())) {
                clear_pending_settings_save(&ui, "Lock screen display request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_wallpaper_source_selected(move |source| {
            if !state.dispatch(Command::SetWallpaperSource(source.to_string())) {
                clear_pending_settings_save(&ui, "Settings request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_start_at_login_changed(move |enabled| {
            if !state.dispatch(Command::SetStartAtLogin(enabled)) {
                clear_pending_settings_save(&ui, "Settings request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_keep_running_in_background_changed(move |enabled| {
            if !state.dispatch(Command::SetKeepRunningInBackground(enabled)) {
                clear_pending_settings_save(&ui, "Settings request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_automatic_update_checks_changed(move |enabled| {
            if !state.dispatch(Command::SetAutomaticUpdateChecks(enabled)) {
                clear_pending_settings_save(&ui, "Settings request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_language_selected(move |language| {
            if !state.dispatch(Command::SetLanguage(language.to_string())) {
                clear_pending_settings_save(&ui, "Settings request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_history_limit_selected(move |limit| {
            let limit = u16::try_from(limit).ok().filter(|limit| *limit > 0);
            if !state.dispatch(Command::SetHistoryLimit(limit)) {
                clear_pending_settings_save(&ui, "Settings request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_sync_interval_selected(move |minutes| {
            if !state.dispatch(Command::SetSyncInterval(minutes.max(1) as u32)) {
                clear_pending_settings_save(&ui, "Settings request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_theme_selected(move |theme| {
            if !state.dispatch(Command::SetTheme(theme.to_string())) {
                clear_pending_settings_save(&ui, "Settings request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_open_data_folder_requested(move || {
            if !state.dispatch(Command::OpenDataFolder) {
                clear_pending_external_action(&ui, "Open folder request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_open_favorites_folder_requested(move || {
            if !state.dispatch(Command::OpenFavoritesFolder) {
                clear_pending_external_action(&ui, "Open folder request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_open_logs_folder_requested(move || {
            if !state.dispatch(Command::OpenLogsFolder) {
                clear_pending_external_action(&ui, "Open folder request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_update_check_requested(move || {
            state.check_for_updates_async(ui.clone(), true);
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_update_action_requested(move || {
            state.handle_update_action(ui.clone());
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_reveal_current_requested(move || {
            if !state.dispatch(Command::RevealCurrentImage) {
                clear_pending_external_action(&ui, "Reveal request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_wallpaper_reveal_requested(move |id| {
            if !state.dispatch(Command::RevealWallpaper { id: id.to_string() }) {
                clear_pending_external_action(&ui, "Reveal request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_wallpaper_export_requested(move |id| {
            if !state.dispatch(Command::ExportWallpaper { id: id.to_string() }) {
                clear_pending_external_action(&ui, "Export request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_wallpaper_info_requested(move |id| {
            if !state.dispatch(Command::OpenWallpaperInfo { id: id.to_string() }) {
                clear_pending_external_action(&ui, "Open web page request failed");
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_wallpaper_sync_requested(move |id| {
            if !state.dispatch(Command::SyncWallpaper { id: id.to_string() }) {
                clear_pending_sync(&ui);
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_wallpaper_selected(move |item| {
            let Some(app) = ui.upgrade() else {
                return;
            };

            state.select_wallpaper(&app, item);
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_wallpaper_remove_requested(move |id| {
            let id = id.to_string();
            let app = ui.upgrade();
            let next_item = app
                .as_ref()
                .and_then(|app| state.adjacent_wallpaper_after_removal(app, id.as_str()));

            if state.dispatch(Command::RemoveWallpaper { id: id.clone() }) {
                if let (Some(app), Some(item)) = (app, next_item) {
                    state.select_wallpaper(&app, item);
                }
            } else {
                clear_pending_remove(&ui, &id);
            }
        });

        let state = self.clone();
        let ui = app.as_weak();
        app.on_favorite_toggled(move |id, favorite| {
            let id = id.to_string();
            if let Some(app) = ui.upgrade() {
                let removed_favorite_row = if !favorite && app.get_favorites_only_enabled() {
                    model_row_by_id(&app.get_favorite_wallpapers(), id.as_str())
                } else {
                    None
                };

                crate::bridge::apply_favorite_optimistic(&app, id.as_str(), favorite);
                if !favorite {
                    state.align_after_favorite_removed(&app, id.as_str(), removed_favorite_row);
                }
            }
            if !state.dispatch(Command::SetFavorite {
                id: id.clone(),
                favorite,
            }) {
                rollback_favorite(&ui, id.as_str(), favorite);
                if !favorite && let Some(app) = ui.upgrade() {
                    state.align_selection_to_visible_filter(&app);
                }
            }
        });

        self.schedule_automatic_update_check(
            app.as_weak(),
            UPDATE_CHECK_DELAY,
            UPDATE_CONFIG_RETRIES,
        );
    }

    fn restore_cached_update_state(&self, app: &MainWindow) {
        match crate::update::cached_update_check(&self.update_dir) {
            Ok(Some(check)) => self.apply_update_check(app, check),
            Ok(None) => set_current_update_status(app),
            Err(error) => {
                tracing::warn!(%error, "failed to load cached update state");
                set_current_update_status(app);
            }
        }
    }

    fn apply_update_check(&self, app: &MainWindow, check: crate::update::UpdateCheck) {
        match check {
            crate::update::UpdateCheck::NoRelease => {
                clear_update_slot(&self.available_update, "available update");
                clear_update_slot(&self.prepared_update, "prepared update");
                app.set_update_status_kind("no-release".into());
                app.set_update_status_version("".into());
                app.set_update_action_kind("check".into());
            }
            crate::update::UpdateCheck::Available { release } => {
                let version = release.version.to_string();
                let ready = self.prepared_update.lock().is_ok_and(|prepared| {
                    prepared
                        .as_ref()
                        .is_some_and(|prepared| prepared.version == release.version)
                });
                replace_update_slot(&self.available_update, release.clone(), "available update");
                app.set_update_status_version(version.into());
                if ready {
                    app.set_update_status_kind("ready".into());
                    app.set_update_action_kind("install".into());
                } else {
                    clear_update_slot(&self.prepared_update, "prepared update");
                    app.set_update_status_kind("available".into());
                    app.set_update_action_kind(
                        if crate::update::release_is_downloadable(&release) {
                            "download"
                        } else {
                            "view"
                        }
                        .into(),
                    );
                }
            }
            crate::update::UpdateCheck::UpToDate { release } => {
                clear_update_slot(&self.available_update, "available update");
                clear_update_slot(&self.prepared_update, "prepared update");
                app.set_update_status_kind("up-to-date".into());
                app.set_update_status_version(release.version.to_string().into());
                app.set_update_action_kind("check".into());
            }
        }
    }

    fn handle_update_action(&self, ui: slint::Weak<MainWindow>) {
        let prepared = match self.prepared_update.lock() {
            Ok(prepared) => prepared.clone(),
            Err(error) => {
                tracing::warn!(%error, "prepared update state was poisoned");
                show_update_feedback(&ui, "Update install could not be started");
                return;
            }
        };
        if let Some(prepared) = prepared {
            self.install_update_async(ui, prepared);
            return;
        }

        let release = match self.available_update.lock() {
            Ok(release) => release.clone(),
            Err(error) => {
                tracing::warn!(%error, "available update state was poisoned");
                show_update_feedback(&ui, "Update download could not be started");
                return;
            }
        };
        if let Some(release) = release
            && crate::update::release_is_downloadable(&release)
        {
            self.download_update_async(ui, release);
            return;
        }

        if !self.dispatch(Command::OpenReleasePage) {
            clear_pending_external_action(&ui, "Open release page request failed");
        }
    }

    fn download_update_async(
        &self,
        ui: slint::Weak<MainWindow>,
        release: crate::update::ReleaseInfo,
    ) -> bool {
        if !self.begin_update_operation() {
            return false;
        }
        let Some(app) = ui.upgrade() else {
            self.finish_update_operation();
            return false;
        };
        app.set_update_status_kind("downloading".into());
        app.set_update_status_version(release.version.to_string().into());
        app.set_update_action_kind("none".into());
        drop(app);

        let state = self.clone();
        let result_ui = ui.clone();
        let spawn = thread::Builder::new()
            .name("spotlit-update-download".to_string())
            .stack_size(BACKGROUND_THREAD_STACK_SIZE)
            .spawn(move || {
                let result = crate::update::download_update(&release, &state.update_dir).and_then(
                    |prepared| {
                        let version = prepared.version.clone();
                        state
                            .prepared_update
                            .lock()
                            .map_err(|error| anyhow::anyhow!("prepared update state: {error}"))?
                            .replace(prepared);
                        Ok(version)
                    },
                );
                if let Err(error) = &result {
                    tracing::warn!(%error, "Spotlit update download failed");
                }
                state.finish_update_operation();

                if let Err(error) = slint::invoke_from_event_loop(move || {
                    let Some(app) = result_ui.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(version) => {
                            app.set_update_status_kind("ready".into());
                            app.set_update_status_version(version.to_string().into());
                            app.set_update_action_kind("install".into());
                        }
                        Err(error) => {
                            app.set_update_status_kind("download-failed".into());
                            app.set_update_action_kind("download".into());
                            crate::ui_events::show_settings_feedback(
                                &app,
                                format!("Update download failed: {error}"),
                            );
                        }
                    }
                }) {
                    tracing::warn!(%error, "failed to post update download result");
                }
            });

        if let Err(error) = spawn {
            self.finish_update_operation();
            if let Some(app) = ui.upgrade() {
                app.set_update_status_kind("download-failed".into());
                app.set_update_action_kind("download".into());
                crate::ui_events::show_settings_feedback(
                    &app,
                    "Update download could not be started",
                );
            }
            tracing::warn!(%error, "failed to start update download thread");
            return false;
        }
        true
    }

    fn install_update_async(
        &self,
        ui: slint::Weak<MainWindow>,
        prepared: crate::update::PreparedUpdate,
    ) -> bool {
        if !self.begin_update_operation() {
            return false;
        }
        let Some(app) = ui.upgrade() else {
            self.finish_update_operation();
            return false;
        };
        app.set_update_status_kind("installing".into());
        app.set_update_action_kind("none".into());
        drop(app);

        let state = self.clone();
        let result_ui = ui.clone();
        let spawn = thread::Builder::new()
            .name("spotlit-update-install".to_string())
            .stack_size(BACKGROUND_THREAD_STACK_SIZE)
            .spawn(move || {
                let result = crate::update::install_prepared_update(&prepared);
                if let Err(error) = &result {
                    tracing::warn!(%error, "Spotlit update install failed");
                }
                state.finish_update_operation();

                if let Err(error) = slint::invoke_from_event_loop(move || {
                    let Some(app) = result_ui.upgrade() else {
                        return;
                    };
                    match result {
                        #[cfg(target_os = "linux")]
                        Ok(crate::update::InstallDisposition::ExternalInstaller) => {
                            app.set_update_status_kind("installer-opened".into());
                            app.set_update_action_kind("install".into());
                            crate::ui_events::show_settings_feedback(
                                &app,
                                "Opened update in the system installer",
                            );
                        }
                        #[cfg(windows)]
                        Ok(crate::update::InstallDisposition::Restarting) => {
                            if let Err(error) = slint::quit_event_loop() {
                                tracing::warn!(%error, "failed to exit for Spotlit update");
                            }
                        }
                        Err(error) => {
                            app.set_update_status_kind("install-failed".into());
                            app.set_update_action_kind("install".into());
                            crate::ui_events::show_settings_feedback(
                                &app,
                                format!("Update install failed: {error}"),
                            );
                        }
                    }
                }) {
                    tracing::warn!(%error, "failed to post update install result");
                }
            });

        if let Err(error) = spawn {
            self.finish_update_operation();
            if let Some(app) = ui.upgrade() {
                app.set_update_status_kind("install-failed".into());
                app.set_update_action_kind("install".into());
                crate::ui_events::show_settings_feedback(
                    &app,
                    "Update install could not be started",
                );
            }
            tracing::warn!(%error, "failed to start update install thread");
            return false;
        }
        true
    }

    fn begin_update_operation(&self) -> bool {
        self.update_operation_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn finish_update_operation(&self) {
        self.update_operation_pending
            .store(false, Ordering::Release);
    }

    fn schedule_automatic_update_check(
        &self,
        ui: slint::Weak<MainWindow>,
        delay: Duration,
        retries: u8,
    ) {
        let state = self.clone();
        slint::Timer::single_shot(delay, move || {
            if state.automatic_update_check_decided.load(Ordering::Acquire) {
                return;
            }

            if ui.upgrade().is_none() {
                return;
            }
            let Some(enabled) = state.automatic_update_checks_enabled() else {
                if retries > 0 {
                    state.schedule_automatic_update_check(
                        ui,
                        UPDATE_CONFIG_RETRY_DELAY,
                        retries - 1,
                    );
                }
                return;
            };

            if state
                .automatic_update_check_decided
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return;
            }

            if enabled {
                match crate::update::automatic_check_due(&state.update_dir) {
                    Ok(true) => {
                        state.check_for_updates_async(ui, false);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(%error, "failed to read update check cadence");
                        state.check_for_updates_async(ui, false);
                    }
                }
            }
        });
    }

    fn check_for_updates_async(&self, ui: slint::Weak<MainWindow>, user_initiated: bool) -> bool {
        if user_initiated {
            self.automatic_update_check_decided
                .store(true, Ordering::Release);
        }
        if !self.begin_update_operation() {
            return false;
        }

        let Some(app) = ui.upgrade() else {
            self.finish_update_operation();
            return false;
        };
        app.set_update_status_kind("checking".into());
        app.set_update_action_kind("none".into());
        drop(app);

        let state = self.clone();
        let result_ui = ui.clone();
        let spawn = thread::Builder::new()
            .name("spotlit-update-check".to_string())
            .stack_size(BACKGROUND_THREAD_STACK_SIZE)
            .spawn(move || {
                let result = crate::update::check_for_update().map_err(|error| error.to_string());
                let cache_result = match &result {
                    Ok(check) => Ok(check.clone()),
                    Err(error) => Err(anyhow::anyhow!(error.clone())),
                };
                if let Err(error) =
                    crate::update::record_check_result(&state.update_dir, &cache_result)
                {
                    tracing::warn!(%error, "failed to persist update check result");
                }
                if let Err(error) = &result {
                    tracing::warn!(%error, "Spotlit update check failed");
                }
                state.finish_update_operation();

                if let Err(error) = slint::invoke_from_event_loop(move || {
                    let Some(app) = result_ui.upgrade() else {
                        return;
                    };

                    match result {
                        Ok(crate::update::UpdateCheck::NoRelease) => {
                            state.apply_update_check(&app, crate::update::UpdateCheck::NoRelease);
                        }
                        Ok(crate::update::UpdateCheck::Available { release }) => {
                            state.apply_update_check(
                                &app,
                                crate::update::UpdateCheck::Available { release },
                            );
                        }
                        Ok(crate::update::UpdateCheck::UpToDate { release }) => {
                            state.apply_update_check(
                                &app,
                                crate::update::UpdateCheck::UpToDate { release },
                            );
                        }
                        Err(error) => {
                            app.set_update_status_kind("check-failed".into());
                            app.set_update_action_kind(state.failed_check_action_kind().into());
                            if user_initiated {
                                crate::ui_events::show_settings_feedback(
                                    &app,
                                    format!("Update check failed: {error}"),
                                );
                            }
                        }
                    }
                }) {
                    tracing::warn!(%error, "failed to post update check result");
                }
            });

        if let Err(error) = spawn {
            self.finish_update_operation();
            if let Some(app) = ui.upgrade() {
                app.set_update_status_kind("check-failed".into());
                app.set_update_action_kind(self.failed_check_action_kind().into());
                if user_initiated {
                    crate::ui_events::show_settings_feedback(
                        &app,
                        "Update check could not be started",
                    );
                }
            }
            tracing::warn!(%error, "failed to start update check thread");
            return false;
        }

        true
    }

    fn failed_check_action_kind(&self) -> &'static str {
        match self.available_update.lock() {
            Ok(release)
                if release
                    .as_ref()
                    .is_some_and(crate::update::release_is_downloadable) =>
            {
                "download"
            }
            Ok(release) if release.is_some() => "view",
            Ok(_) => "check",
            Err(error) => {
                tracing::warn!(%error, "available update state was poisoned");
                "check"
            }
        }
    }

    pub fn refresh_async(&self) -> bool {
        self.mark_present_refresh();
        self.dispatch(Command::Scan)
    }

    pub fn refresh_after_present(&self, ui: slint::Weak<MainWindow>) {
        if self.should_refresh_after_present() {
            let state = self.clone();
            slint::Timer::single_shot(PRESENT_REFRESH_DELAY, move || {
                let Some(app) = ui.upgrade() else {
                    state.clear_present_refresh();
                    return;
                };

                if !app.window().is_visible() || app.window().is_minimized() {
                    state.clear_present_refresh();
                    return;
                }

                state.dispatch(Command::Scan);
            });
        }
    }

    pub fn apply_cached_snapshot(&self, app: &MainWindow) -> bool {
        let snapshot = match self.latest_snapshot.lock() {
            Ok(snapshot) => snapshot.clone(),
            Err(error) => {
                tracing::warn!(%error, "latest snapshot cache was poisoned");
                None
            }
        };

        if let Some(snapshot) = snapshot {
            crate::bridge::apply_prepared_snapshot(app, &snapshot);
            diagnostics::apply_to_ui(app);
            true
        } else {
            false
        }
    }

    pub(crate) fn keep_running_in_background(&self) -> Option<bool> {
        self.cached_config()
            .map(|config| config.keep_running_in_background)
    }

    fn automatic_update_checks_enabled(&self) -> Option<bool> {
        self.cached_config()
            .map(|config| config.automatic_update_checks)
    }

    fn cached_config(&self) -> Option<AppConfig> {
        match self.latest_snapshot.lock() {
            Ok(snapshot) => snapshot.as_ref().map(|snapshot| snapshot.config.clone()),
            Err(error) => {
                tracing::warn!(%error, "latest snapshot cache was poisoned");
                None
            }
        }
    }

    pub fn apply_cached_snapshot_after_present(&self, ui: slint::Weak<MainWindow>) {
        let state = self.clone();
        let generation = self
            .present_snapshot_generation
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        let should_load_snapshot = !self.has_cached_snapshot();
        if should_load_snapshot {
            self.request_snapshot_load_after_present(ui.clone(), generation);
        }

        slint::Timer::single_shot(CACHED_SNAPSHOT_APPLY_DELAY, move || {
            if state.present_snapshot_generation.load(Ordering::Acquire) != generation {
                return;
            }

            let Some(app) = ui.upgrade() else {
                return;
            };

            if !app.window().is_visible() || app.window().is_minimized() {
                return;
            }

            if !state.apply_cached_snapshot(&app) && !should_load_snapshot {
                state.request_snapshot_load_once();
            }
            if window_accepts_image_work(&app) {
                crate::ui_events::request_visible_images(&app);
            }
        });
    }

    pub fn cancel_window_work(&self) {
        self.present_snapshot_generation
            .fetch_add(1, Ordering::AcqRel);
        self.preview_generation.fetch_add(1, Ordering::AcqRel);
        self.preview_loader.cancel_pending();
        self.snapshot_load_pending.store(false, Ordering::Release);
    }

    fn request_snapshot_load_after_present(&self, ui: slint::Weak<MainWindow>, generation: u64) {
        let state = self.clone();
        slint::Timer::single_shot(SNAPSHOT_LOAD_AFTER_PRESENT_DELAY, move || {
            if state.present_snapshot_generation.load(Ordering::Acquire) != generation {
                return;
            }

            let Some(app) = ui.upgrade() else {
                return;
            };

            if !app.window().is_visible() || app.window().is_minimized() {
                return;
            }

            state.request_snapshot_load_once();
        });
    }

    pub fn start_scheduler(&self) -> io::Result<()> {
        let state = self.clone();
        std::thread::Builder::new()
            .name("spotlit-scheduler".to_string())
            .stack_size(BACKGROUND_THREAD_STACK_SIZE)
            .spawn(move || {
                let (enabled, changed) = &*state.auto_sync_state;
                let mut enabled = match enabled.lock() {
                    Ok(enabled) => enabled,
                    Err(error) => {
                        tracing::warn!(%error, "auto-sync scheduler state was poisoned");
                        return;
                    }
                };

                loop {
                    while !*enabled {
                        enabled = match changed.wait(enabled) {
                            Ok(enabled) => enabled,
                            Err(error) => {
                                tracing::warn!(%error, "auto-sync scheduler wait failed");
                                return;
                            }
                        };
                    }

                    let wait_result = changed.wait_timeout(enabled, SCHEDULER_TICK);
                    let (next_enabled, timeout) = match wait_result {
                        Ok(result) => result,
                        Err(error) => {
                            tracing::warn!(%error, "auto-sync scheduler wait failed");
                            return;
                        }
                    };
                    enabled = next_enabled;

                    if *enabled && timeout.timed_out() {
                        drop(enabled);
                        state.dispatch(Command::AutoSyncTick);
                        enabled = match state.auto_sync_state.0.lock() {
                            Ok(enabled) => enabled,
                            Err(error) => {
                                tracing::warn!(%error, "auto-sync scheduler state was poisoned");
                                return;
                            }
                        };
                    }
                }
            })
            .map(|_| ())
    }

    fn dispatch(&self, command: Command) -> bool {
        match self.worker.dispatch(command) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(error = %error, "failed to send command to worker thread");
                false
            }
        }
    }

    fn has_cached_snapshot(&self) -> bool {
        match self.latest_snapshot.lock() {
            Ok(snapshot) => snapshot.is_some(),
            Err(error) => {
                tracing::warn!(%error, "latest snapshot cache was poisoned");
                false
            }
        }
    }

    fn request_snapshot_load_once(&self) {
        if self
            .snapshot_load_pending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.dispatch(Command::LoadSnapshot);
        }
    }

    fn mark_present_refresh(&self) {
        match self.last_present_refresh.lock() {
            Ok(mut refreshed_at) => {
                *refreshed_at = Some(Instant::now());
            }
            Err(error) => {
                tracing::warn!(%error, "present refresh state was poisoned");
            }
        }
    }

    fn clear_present_refresh(&self) {
        match self.last_present_refresh.lock() {
            Ok(mut refreshed_at) => {
                *refreshed_at = None;
            }
            Err(error) => {
                tracing::warn!(%error, "present refresh state was poisoned");
            }
        }
    }

    fn should_refresh_after_present(&self) -> bool {
        match self.last_present_refresh.lock() {
            Ok(mut refreshed_at) => {
                let now = Instant::now();
                let should_refresh = refreshed_at.as_ref().is_none_or(|refreshed_at| {
                    now.duration_since(*refreshed_at) >= PRESENT_REFRESH_COOLDOWN
                });
                if should_refresh {
                    *refreshed_at = Some(now);
                }
                should_refresh
            }
            Err(error) => {
                tracing::warn!(%error, "present refresh state was poisoned");
                true
            }
        }
    }

    fn load_selected_preview_async(&self, id: String, path: PathBuf) {
        let generation = self.preview_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let path_text = path.to_string_lossy().into_owned();
        let request = PreviewLoadRequest {
            id,
            path,
            path_text,
            generation,
        };

        self.preview_loader.queue(request);
    }

    fn navigate_wallpaper(&self, app: &MainWindow, delta: i32) {
        let model = if app.get_favorites_only_enabled() {
            app.get_favorite_wallpapers()
        } else {
            app.get_wallpapers()
        };

        let Some(item) = next_visible_wallpaper(app, &model, delta) else {
            return;
        };
        if same_active_wallpaper(app, &item) {
            return;
        }

        self.select_wallpaper(app, item);
    }

    fn select_library_filter(&self, app: &MainWindow, favorites_only: bool) {
        if app.get_favorites_only_enabled() == favorites_only {
            return;
        }

        app.set_favorites_only_enabled(favorites_only);
        app.set_pending_remove_id(SharedString::default());
        self.align_selection_to_visible_filter(app);
        crate::ui_events::request_visible_images(app);
    }

    fn align_selection_to_visible_filter(&self, app: &MainWindow) {
        let model = if app.get_favorites_only_enabled() {
            app.get_favorite_wallpapers()
        } else {
            app.get_wallpapers()
        };
        let removing_id = app.get_removing_wallpaper_id();

        if model_row_by_id_except(
            &model,
            active_wallpaper_id(app).as_str(),
            removing_id.as_str(),
        )
        .is_some()
        {
            return;
        }

        if let Some(item) = first_selectable_wallpaper(&model, removing_id.as_str()) {
            self.select_wallpaper(app, item);
        } else if app.get_has_current() && app.get_current_id().as_str() != removing_id.as_str() {
            show_current_wallpaper(app);
        } else {
            clear_active_wallpaper(app);
        }
    }

    fn align_after_favorite_removed(
        &self,
        app: &MainWindow,
        removed_id: &str,
        removed_row: Option<usize>,
    ) {
        if !app.get_favorites_only_enabled()
            || active_wallpaper_id(app).as_str() != removed_id
            || model_row_by_id(&app.get_favorite_wallpapers(), removed_id).is_some()
        {
            return;
        }

        let favorites = app.get_favorite_wallpapers();
        let row_count = favorites.row_count();
        if row_count == 0 {
            if app.get_has_current() {
                show_current_wallpaper(app);
            } else {
                clear_active_wallpaper(app);
            }
            return;
        }

        let target_row = removed_row.unwrap_or(0).min(row_count.saturating_sub(1));
        if let Some(item) = favorites.row_data(target_row) {
            self.select_wallpaper(app, item);
        }
    }

    fn adjacent_wallpaper_after_removal(
        &self,
        app: &MainWindow,
        removed_id: &str,
    ) -> Option<WallpaperItem> {
        if active_wallpaper_id(app).as_str() != removed_id {
            return None;
        }

        let model = if app.get_favorites_only_enabled() {
            app.get_favorite_wallpapers()
        } else {
            app.get_wallpapers()
        };
        let removed_row = model_row_by_id(&model, removed_id)?;
        let row_count = model.row_count();
        if row_count <= 1 {
            return None;
        }

        let target_row = if removed_row + 1 < row_count {
            removed_row + 1
        } else {
            removed_row.saturating_sub(1)
        };
        model.row_data(target_row)
    }

    fn select_wallpaper(&self, app: &MainWindow, item: WallpaperItem) {
        if item.id == app.get_current_id() {
            show_current_wallpaper(app);
            return;
        }

        if same_active_wallpaper(app, &item) {
            app.set_pending_remove_id(SharedString::default());
            return;
        }

        app.set_show_current_details(false);
        clear_transient_action_feedback(app);

        apply_selected_preview_placeholder(app, &item);
        app.set_selected_wallpaper(item.clone());
        app.set_has_selection(true);
        app.set_pending_remove_id(SharedString::default());

        let preview_source = preview_source(&item);
        if preview_source.is_empty()
            || (item.id == app.get_current_id()
                && preview_source == app.get_current_preview_source_path())
        {
            return;
        }

        let id = item.id.to_string();
        let path = preview_source.to_string();
        let ui = app.as_weak();
        if !apply_cached_selected_preview(&ui, &id, &path) {
            self.load_selected_preview_async(id, PathBuf::from(path));
        }
    }
}

fn set_current_update_status(app: &MainWindow) {
    app.set_update_status_kind("current".into());
    app.set_update_status_version(env!("CARGO_PKG_VERSION").into());
    app.set_update_action_kind("check".into());
}

fn clear_update_slot<T>(slot: &Mutex<Option<T>>, label: &'static str) {
    match slot.lock() {
        Ok(mut value) => {
            *value = None;
        }
        Err(error) => tracing::warn!(%error, label, "update state was poisoned"),
    }
}

fn replace_update_slot<T>(slot: &Mutex<Option<T>>, value: T, label: &'static str) {
    match slot.lock() {
        Ok(mut current) => {
            *current = Some(value);
        }
        Err(error) => tracing::warn!(%error, label, "update state was poisoned"),
    }
}

fn show_update_feedback(ui: &slint::Weak<MainWindow>, message: &'static str) {
    if let Some(app) = ui.upgrade() {
        crate::ui_events::show_settings_feedback(&app, message);
    }
}

impl PreviewLoaderHandle {
    fn new(ui: UiSink, generation: Arc<AtomicU64>) -> Self {
        let state = Arc::new(PreviewLoaderState {
            pending: Mutex::new(None),
            last_request: Mutex::new(None),
            changed: Condvar::new(),
        });

        Self {
            state,
            ui,
            generation,
            started: Arc::new(AtomicBool::new(false)),
        }
    }

    fn queue(&self, request: PreviewLoadRequest) {
        if self.remember_request(&request) {
            return;
        }

        let queued = match self.state.pending.lock() {
            Ok(mut pending) => {
                *pending = Some(request);
                self.state.changed.notify_one();
                true
            }
            Err(error) => {
                tracing::warn!(%error, "selected preview loader queue was poisoned");
                false
            }
        };

        if queued {
            self.ensure_started();
        }
    }

    fn ensure_started(&self) {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let ui = self.ui.clone();
        let generation = Arc::clone(&self.generation);
        let loader_state = Arc::clone(&self.state);
        if let Err(error) = std::thread::Builder::new()
            .name("spotlit-selected-preview-loader".to_string())
            .stack_size(IMAGE_THREAD_STACK_SIZE)
            .spawn(move || {
                run_preview_loader(ui, generation, loader_state);
            })
        {
            self.started.store(false, Ordering::Release);
            tracing::warn!(%error, "failed to spawn selected preview loader");
        }
    }

    fn remember_request(&self, request: &PreviewLoadRequest) -> bool {
        match self.state.last_request.lock() {
            Ok(mut last_request) => {
                let key = request.key();
                if last_request.as_ref() == Some(&key) {
                    return true;
                }

                *last_request = Some(key);
                false
            }
            Err(error) => {
                tracing::warn!(%error, "selected preview request cache was poisoned");
                false
            }
        }
    }

    fn cancel_pending(&self) {
        match self.state.pending.lock() {
            Ok(mut pending) => {
                *pending = None;
            }
            Err(error) => {
                tracing::warn!(%error, "selected preview loader queue was poisoned");
            }
        }

        match self.state.last_request.lock() {
            Ok(mut last_request) => {
                *last_request = None;
            }
            Err(error) => {
                tracing::warn!(%error, "selected preview request cache was poisoned");
            }
        }

        self.state.changed.notify_all();
    }
}

impl PreviewLoadRequest {
    fn key(&self) -> PreviewRequestKey {
        PreviewRequestKey {
            id: self.id.clone(),
            path_text: self.path_text.clone(),
        }
    }
}

fn run_preview_loader(ui: UiSink, generation: Arc<AtomicU64>, state: Arc<PreviewLoaderState>) {
    loop {
        let mut pending = match state.pending.lock() {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "selected preview loader queue was poisoned");
                return;
            }
        };

        while pending.is_none() {
            pending = match state.changed.wait(pending) {
                Ok(pending) => pending,
                Err(error) => {
                    tracing::warn!(%error, "selected preview loader wait failed");
                    return;
                }
            };
        }

        let Some(request) = pending.take() else {
            continue;
        };
        drop(pending);

        if generation.load(Ordering::Relaxed) != request.generation {
            continue;
        }

        let started_at = Instant::now();
        let Some(image) = decode_display_image(&request.path) else {
            tracing::warn!(
                id = %request.id,
                path = %request.path.display(),
                "failed to decode selected preview"
            );
            clear_last_preview_request(&state, &request);
            continue;
        };
        diagnostics::record(Metric::CurrentPreview, started_at.elapsed());
        if generation.load(Ordering::Acquire) != request.generation {
            clear_last_preview_request(&state, &request);
            continue;
        }

        post_selected_preview(
            ui.clone(),
            Arc::clone(&generation),
            ReadySelectedPreview { request, image },
            SELECTED_PREVIEW_RESULT_RETRIES,
        );
        clear_active_preview_request(&state);
    }
}

fn post_selected_preview(
    ui: UiSink,
    latest_generation: Arc<AtomicU64>,
    ready: ReadySelectedPreview,
    retries: u8,
) {
    if let Err(error) = slint::invoke_from_event_loop(move || {
        if latest_generation.load(Ordering::Acquire) != ready.request.generation {
            return;
        }

        let Some(app) = ui.current().and_then(|ui| ui.upgrade()) else {
            return;
        };
        if !app.window().is_visible() || app.window().is_minimized() {
            return;
        }
        if !window_accepts_image_work(&app) {
            if retries > 0 {
                slint::Timer::single_shot(SELECTED_PREVIEW_RESULT_RETRY_DELAY, move || {
                    post_selected_preview(ui, latest_generation, ready, retries - 1);
                });
            }
            return;
        }

        let selected = app.get_selected_wallpaper();
        if !app.get_has_selection()
            || selected.id.as_str() != ready.request.id
            || !selected_matches_preview_source(&selected, &ready.request.path_text)
        {
            return;
        }

        let image = ready.image.into_slint_image();
        image_cache::remember_display_preview(&ready.request.id, &ready.request.path_text, &image);
        crate::bridge::apply_selected_preview(&app, image, ready.request.path_text.into());
        diagnostics::apply_to_ui(&app);
    }) {
        tracing::warn!(%error, "failed to queue selected preview on UI event loop");
    }
}

fn clear_last_preview_request(state: &PreviewLoaderState, request: &PreviewLoadRequest) {
    match state.last_request.lock() {
        Ok(mut last_request) => {
            if last_request.as_ref() == Some(&request.key()) {
                *last_request = None;
            }
        }
        Err(error) => {
            tracing::warn!(%error, "selected preview request cache was poisoned");
        }
    }
}

fn clear_active_preview_request(state: &PreviewLoaderState) {
    match state.last_request.lock() {
        Ok(mut last_request) => {
            *last_request = None;
        }
        Err(error) => {
            tracing::warn!(%error, "selected preview request cache was poisoned");
        }
    }
}

fn apply_cached_selected_preview(ui: &slint::Weak<MainWindow>, id: &str, path: &str) -> bool {
    let Some(image) = image_cache::display_preview(id, path) else {
        return false;
    };

    let Some(app) = ui.upgrade() else {
        return false;
    };

    let selected = app.get_selected_wallpaper();
    if !app.get_has_selection()
        || selected.id.as_str() != id
        || !selected_matches_preview_source(&selected, path)
    {
        return false;
    }

    crate::bridge::apply_selected_preview(&app, image, path.into());
    diagnostics::apply_to_ui(&app);
    true
}

fn cache_latest_snapshot(
    cache: &Arc<Mutex<Option<Arc<PreparedSnapshot>>>>,
    event: &PreparedWorkerEvent,
) {
    let snapshot = match event {
        PreparedWorkerEvent::Snapshot(snapshot)
        | PreparedWorkerEvent::Synced(_, snapshot)
        | PreparedWorkerEvent::FavoriteUpdated(_, snapshot)
        | PreparedWorkerEvent::SettingsUpdated(_, snapshot) => Some(Arc::clone(snapshot)),
        PreparedWorkerEvent::ConfigUpdated(_, settings) => {
            Some(latest_settings_snapshot(cache, settings))
        }
        PreparedWorkerEvent::AutoSyncIdle
        | PreparedWorkerEvent::SnapshotUnchanged
        | PreparedWorkerEvent::OpenedPath(_)
        | PreparedWorkerEvent::Failed(_) => None,
    };

    let Some(snapshot) = snapshot else {
        return;
    };

    match cache.lock() {
        Ok(mut cache) => {
            *cache = Some(snapshot);
        }
        Err(error) => {
            tracing::warn!(%error, "latest snapshot cache was poisoned");
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerEventPostDecision {
    Full,
    SnapshotUnchanged,
}

fn worker_event_post_decision(
    cache: &Arc<Mutex<Option<Arc<PreparedSnapshot>>>>,
    event: &PreparedWorkerEvent,
) -> WorkerEventPostDecision {
    let PreparedWorkerEvent::Snapshot(snapshot) = event else {
        return WorkerEventPostDecision::Full;
    };

    match cache.lock() {
        Ok(cache) => {
            if cache
                .as_ref()
                .is_none_or(|cached| cached.ui_signature != snapshot.ui_signature)
            {
                WorkerEventPostDecision::Full
            } else {
                WorkerEventPostDecision::SnapshotUnchanged
            }
        }
        Err(error) => {
            tracing::warn!(%error, "latest snapshot cache was poisoned");
            WorkerEventPostDecision::Full
        }
    }
}

fn clear_snapshot_load_pending(pending: &AtomicBool, event: &PreparedWorkerEvent) {
    match event {
        PreparedWorkerEvent::Snapshot(_) | PreparedWorkerEvent::Failed(_) => {
            pending.store(false, Ordering::Release);
        }
        PreparedWorkerEvent::Synced(_, _)
        | PreparedWorkerEvent::SnapshotUnchanged
        | PreparedWorkerEvent::FavoriteUpdated(_, _)
        | PreparedWorkerEvent::SettingsUpdated(_, _)
        | PreparedWorkerEvent::ConfigUpdated(_, _)
        | PreparedWorkerEvent::AutoSyncIdle
        | PreparedWorkerEvent::OpenedPath(_) => {}
    }
}

fn update_scheduler_from_event(state: &Arc<(Mutex<bool>, Condvar)>, event: &PreparedWorkerEvent) {
    let enabled = match event {
        PreparedWorkerEvent::Snapshot(snapshot)
        | PreparedWorkerEvent::Synced(_, snapshot)
        | PreparedWorkerEvent::FavoriteUpdated(_, snapshot)
        | PreparedWorkerEvent::SettingsUpdated(_, snapshot) => {
            Some(snapshot.config.auto_sync_lock_screen)
        }
        PreparedWorkerEvent::ConfigUpdated(_, settings) => {
            Some(settings.config.auto_sync_lock_screen)
        }
        PreparedWorkerEvent::Failed(_) => None,
        PreparedWorkerEvent::AutoSyncIdle
        | PreparedWorkerEvent::SnapshotUnchanged
        | PreparedWorkerEvent::OpenedPath(_) => None,
    };

    let Some(enabled) = enabled else {
        return;
    };

    let (current, changed) = &**state;
    match current.lock() {
        Ok(mut current) => {
            if *current != enabled {
                *current = enabled;
                changed.notify_one();
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to update scheduler state from worker event");
        }
    }
}

fn latest_settings_snapshot(
    cache: &Arc<Mutex<Option<Arc<PreparedSnapshot>>>>,
    settings: &SettingsSnapshot,
) -> Arc<PreparedSnapshot> {
    match cache.lock() {
        Ok(snapshot) => snapshot
            .clone()
            .map(|snapshot| Arc::new(snapshot.with_settings(settings)))
            .unwrap_or_else(|| Arc::new(PreparedSnapshot::settings_only(settings))),
        Err(error) => {
            tracing::warn!(%error, "latest snapshot cache was poisoned");
            Arc::new(PreparedSnapshot::settings_only(settings))
        }
    }
}

fn selected_matches_preview_source(item: &crate::WallpaperItem, path: &str) -> bool {
    item.preview_path.as_str() == path || item.image_path.as_str() == path
}

fn next_visible_wallpaper(
    app: &MainWindow,
    model: &ModelRc<WallpaperItem>,
    delta: i32,
) -> Option<WallpaperItem> {
    if model.row_count() == 0 || delta == 0 {
        return None;
    }

    let removing_id = app.get_removing_wallpaper_id();
    let active_id = active_wallpaper_id(app);
    let active_row = model_row_by_id(model, active_id.as_str());
    let target_row =
        target_selectable_navigation_row(model, removing_id.as_str(), active_row, delta)?;

    model.row_data(target_row)
}

fn active_wallpaper_id(app: &MainWindow) -> SharedString {
    if app.get_has_selection() {
        app.get_selected_wallpaper().id
    } else {
        app.get_current_id()
    }
}

fn target_selectable_navigation_row(
    model: &ModelRc<WallpaperItem>,
    excluded_id: &str,
    active_row: Option<usize>,
    delta: i32,
) -> Option<usize> {
    if delta >= EDGE_NAVIGATION_DELTA {
        return last_selectable_model_row(model, excluded_id);
    }

    if delta <= -EDGE_NAVIGATION_DELTA {
        return first_selectable_model_row(model, excluded_id);
    }

    let Some(active_row) = active_row else {
        return target_initial_selectable_navigation_row(model, excluded_id, delta);
    };

    if model_row_is_selectable(model, active_row, excluded_id) {
        return target_relative_selectable_navigation_row(model, excluded_id, active_row, delta);
    }

    target_nearest_selectable_navigation_row(model, excluded_id, active_row, delta)
}

fn target_initial_selectable_navigation_row(
    model: &ModelRc<WallpaperItem>,
    excluded_id: &str,
    delta: i32,
) -> Option<usize> {
    if delta < 0 {
        last_selectable_model_row(model, excluded_id)
    } else {
        first_selectable_model_row(model, excluded_id)
    }
}

fn target_relative_selectable_navigation_row(
    model: &ModelRc<WallpaperItem>,
    excluded_id: &str,
    active_row: usize,
    delta: i32,
) -> Option<usize> {
    let mut row = active_row;
    let mut remaining = delta.unsigned_abs();

    while remaining > 0 {
        let Some(next_row) = next_selectable_model_row(model, excluded_id, row, delta) else {
            return Some(row);
        };
        row = next_row;
        remaining -= 1;
    }

    Some(row)
}

fn target_nearest_selectable_navigation_row(
    model: &ModelRc<WallpaperItem>,
    excluded_id: &str,
    active_row: usize,
    delta: i32,
) -> Option<usize> {
    if delta < 0 {
        previous_selectable_model_row(model, excluded_id, active_row)
            .or_else(|| first_selectable_model_row(model, excluded_id))
    } else {
        following_selectable_model_row(model, excluded_id, active_row)
            .or_else(|| last_selectable_model_row(model, excluded_id))
    }
}

fn next_selectable_model_row(
    model: &ModelRc<WallpaperItem>,
    excluded_id: &str,
    active_row: usize,
    delta: i32,
) -> Option<usize> {
    if delta < 0 {
        previous_selectable_model_row(model, excluded_id, active_row)
    } else {
        following_selectable_model_row(model, excluded_id, active_row)
    }
}

fn first_selectable_model_row(model: &ModelRc<WallpaperItem>, excluded_id: &str) -> Option<usize> {
    (0..model.row_count()).find(|&row| model_row_is_selectable(model, row, excluded_id))
}

fn last_selectable_model_row(model: &ModelRc<WallpaperItem>, excluded_id: &str) -> Option<usize> {
    (0..model.row_count())
        .rev()
        .find(|&row| model_row_is_selectable(model, row, excluded_id))
}

fn following_selectable_model_row(
    model: &ModelRc<WallpaperItem>,
    excluded_id: &str,
    active_row: usize,
) -> Option<usize> {
    (active_row.saturating_add(1)..model.row_count())
        .find(|&row| model_row_is_selectable(model, row, excluded_id))
}

fn previous_selectable_model_row(
    model: &ModelRc<WallpaperItem>,
    excluded_id: &str,
    active_row: usize,
) -> Option<usize> {
    if active_row == 0 {
        return None;
    }

    (0..active_row)
        .rev()
        .find(|&row| model_row_is_selectable(model, row, excluded_id))
}

fn model_row_is_selectable(model: &ModelRc<WallpaperItem>, row: usize, excluded_id: &str) -> bool {
    model
        .row_data(row)
        .is_some_and(|item| is_selectable_wallpaper(&item, excluded_id))
}

fn first_selectable_wallpaper(
    model: &ModelRc<WallpaperItem>,
    excluded_id: &str,
) -> Option<WallpaperItem> {
    first_selectable_model_row(model, excluded_id).and_then(|row| model.row_data(row))
}

fn model_row_by_id_except(
    model: &ModelRc<WallpaperItem>,
    id: &str,
    excluded_id: &str,
) -> Option<usize> {
    if id.is_empty() || id == excluded_id {
        return None;
    }

    model_row_by_id(model, id)
}

fn is_selectable_wallpaper(item: &WallpaperItem, excluded_id: &str) -> bool {
    excluded_id.is_empty() || item.id.as_str() != excluded_id
}

fn model_row_by_id(model: &ModelRc<WallpaperItem>, id: &str) -> Option<usize> {
    if id.is_empty() {
        return None;
    }

    for row in 0..model.row_count() {
        let Some(item) = model.row_data(row) else {
            continue;
        };

        if item.id.as_str() == id {
            return Some(row);
        }
    }

    None
}

fn same_active_wallpaper(app: &MainWindow, item: &WallpaperItem) -> bool {
    if app.get_has_selection() {
        app.get_selected_wallpaper().id == item.id
    } else {
        app.get_current_id() == item.id
    }
}

fn show_current_wallpaper(app: &MainWindow) {
    if current_wallpaper_is_removing(app) {
        clear_active_wallpaper(app);
        return;
    }

    if app.get_has_selection() && app.get_selected_wallpaper().id != app.get_current_id() {
        clear_transient_action_feedback(app);
    }
    app.set_has_selection(false);
    app.set_show_current_details(false);
    app.set_pending_remove_id(SharedString::default());
}

fn current_wallpaper_is_removing(app: &MainWindow) -> bool {
    let removing_id = app.get_removing_wallpaper_id();
    !removing_id.is_empty() && removing_id == app.get_current_id()
}

fn clear_active_wallpaper(app: &MainWindow) {
    clear_transient_action_feedback(app);
    app.set_has_selection(false);
    app.set_show_current_details(false);
    app.set_pending_remove_id(SharedString::default());
    crate::bridge::clear_selected_preview(app);
}

fn clear_transient_action_feedback(app: &MainWindow) {
    if !has_pending_action_feedback(app) {
        crate::ui_events::clear_action_feedback(app);
    }
}

fn has_pending_action_feedback(app: &MainWindow) -> bool {
    app.get_refresh_pending()
        || app.get_import_pending()
        || app.get_sync_pending()
        || app.get_external_action_pending()
        || !app.get_removing_wallpaper_id().is_empty()
        || !app.get_favorite_pending_id().is_empty()
        || app.get_settings_save_pending()
        || app.get_clean_cache_pending()
}

fn apply_selected_preview_placeholder(app: &MainWindow, item: &WallpaperItem) {
    let preview_source = preview_source(item);

    if item.id == app.get_current_id() && app.get_current_preview_ready() {
        app.set_selected_preview(app.get_current_preview());
        app.set_selected_preview_ready(true);
        app.set_selected_preview_path(app.get_current_preview_path());
    } else if item.thumbnail_ready {
        app.set_selected_preview(item.thumbnail.clone());
        app.set_selected_preview_ready(true);
        app.set_selected_preview_path(preview_source);
    } else if !(app.get_has_selection()
        && app.get_selected_wallpaper().id == item.id
        && app.get_selected_preview_ready())
    {
        crate::bridge::clear_selected_preview(app);
    }
}

fn preview_source(item: &WallpaperItem) -> SharedString {
    if item.preview_path.is_empty() {
        item.image_path.clone()
    } else {
        item.preview_path.clone()
    }
}

fn clear_pending_sync(ui: &slint::Weak<MainWindow>) {
    let Some(app) = ui.upgrade() else {
        return;
    };

    app.set_sync_pending(false);
    app.set_sync_pending_id("".into());
    crate::ui_events::show_action_feedback(&app, "Sync request failed");
}

fn clear_pending_refresh(ui: &slint::Weak<MainWindow>) {
    let Some(app) = ui.upgrade() else {
        return;
    };

    app.set_refresh_pending(false);
    crate::ui_events::show_action_feedback(&app, "Refresh request failed");
}

fn clear_pending_import(ui: &slint::Weak<MainWindow>) {
    let Some(app) = ui.upgrade() else {
        return;
    };

    app.set_import_pending(false);
    crate::ui_events::show_action_feedback(&app, "Import request failed");
}

fn clear_pending_clean_cache(ui: &slint::Weak<MainWindow>) {
    let Some(app) = ui.upgrade() else {
        return;
    };

    app.set_clean_cache_pending(false);
    crate::ui_events::show_settings_operation_feedback(&app, "Clean cache request failed");
}

fn clear_pending_remove(ui: &slint::Weak<MainWindow>, id: &str) {
    let Some(app) = ui.upgrade() else {
        return;
    };

    if app.get_removing_wallpaper_id().as_str() == id {
        app.set_removing_wallpaper_id("".into());
    }
    crate::ui_events::show_action_feedback(&app, "Remove request failed");
}

fn clear_pending_external_action(ui: &slint::Weak<MainWindow>, message: &'static str) {
    let Some(app) = ui.upgrade() else {
        return;
    };

    app.set_external_action_pending(false);
    app.set_external_action_key("".into());
    app.set_external_action_id("".into());
    if matches!(
        message,
        "Open folder request failed" | "Open release page request failed"
    ) {
        crate::ui_events::show_settings_operation_feedback(&app, message);
    } else {
        crate::ui_events::show_action_feedback(&app, message);
    }
}

fn clear_pending_settings_save(ui: &slint::Weak<MainWindow>, message: &'static str) {
    let Some(app) = ui.upgrade() else {
        return;
    };

    app.set_settings_save_pending(false);
    crate::ui_events::show_settings_operation_feedback(&app, message);
}

fn rollback_favorite(ui: &slint::Weak<MainWindow>, id: &str, attempted_favorite: bool) {
    let Some(app) = ui.upgrade() else {
        return;
    };

    if app.get_favorite_pending_id().as_str() == id {
        app.set_favorite_pending_id("".into());
    }
    crate::bridge::apply_favorite_optimistic(&app, id, !attempted_favorite);
    crate::ui_events::show_action_feedback(&app, "Favorite request failed");
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{Arc, Condvar, Mutex, atomic::AtomicU64},
    };

    use crate::core::AppConfig;

    use crate::bridge::PreparedSnapshot;
    use crate::command::Command;
    use crate::platform::{LockScreenIntegration, SystemTheme};
    use crate::ui_events::UiSink;
    use crate::worker_event::CommandFailure;

    use super::{
        PreviewLoadRequest, PreviewLoaderHandle, WorkerEventPostDecision,
        clear_active_preview_request, clear_last_preview_request, update_scheduler_from_event,
        worker_event_post_decision,
    };

    #[test]
    fn selected_preview_request_is_only_suppressed_while_active() {
        let loader = test_preview_loader();
        let request = preview_request("id", "preview.jpg");

        assert!(!loader.remember_request(&request));
        assert!(loader.remember_request(&request));

        clear_last_preview_request(&loader.state, &request);

        assert!(!loader.remember_request(&request));
    }

    #[test]
    fn clearing_active_preview_request_allows_same_request_again() {
        let loader = test_preview_loader();
        let request = preview_request("id", "preview.jpg");

        assert!(!loader.remember_request(&request));
        clear_active_preview_request(&loader.state);

        assert!(!loader.remember_request(&request));
    }

    #[test]
    fn canceling_selected_preview_loader_allows_same_request_again() {
        let loader = test_preview_loader();
        let request = preview_request("id", "preview.jpg");

        assert!(!loader.remember_request(&request));
        loader.cancel_pending();

        assert!(!loader.remember_request(&request));
    }

    #[test]
    fn duplicate_snapshot_event_posts_lightweight_completion() {
        let snapshot = Arc::new(test_snapshot(42));
        let cache = Arc::new(Mutex::new(Some(Arc::clone(&snapshot))));

        let decision =
            worker_event_post_decision(&cache, &super::PreparedWorkerEvent::Snapshot(snapshot));

        assert_eq!(decision, WorkerEventPostDecision::SnapshotUnchanged);
    }

    #[test]
    fn changed_snapshot_event_posts_full_snapshot() {
        let cache = Arc::new(Mutex::new(Some(Arc::new(test_snapshot(42)))));
        let snapshot = Arc::new(test_snapshot(43));

        let decision =
            worker_event_post_decision(&cache, &super::PreparedWorkerEvent::Snapshot(snapshot));

        assert_eq!(decision, WorkerEventPostDecision::Full);
    }

    #[test]
    fn failed_auto_sync_setting_does_not_change_scheduler_state() {
        let state = Arc::new((Mutex::new(false), Condvar::new()));
        let event = super::PreparedWorkerEvent::Failed(CommandFailure {
            command: Command::SetAutoSync(true),
            message: "sync failed".to_string(),
        });

        update_scheduler_from_event(&state, &event);

        assert!(!*state.0.lock().expect("scheduler state"));
    }

    fn test_preview_loader() -> PreviewLoaderHandle {
        PreviewLoaderHandle::new(UiSink::default(), Arc::new(AtomicU64::new(0)))
    }

    fn preview_request(id: &str, path: &str) -> PreviewLoadRequest {
        PreviewLoadRequest {
            id: id.to_string(),
            path: PathBuf::from(path),
            path_text: path.to_string(),
            generation: 1,
        }
    }

    fn test_snapshot(ui_signature: u64) -> PreparedSnapshot {
        PreparedSnapshot {
            current: None,
            wallpapers: Vec::new().into(),
            library_count: 0,
            favorite_count: 0,
            config: AppConfig::default(),
            system_theme: SystemTheme::Light,
            lock_screen_integration: LockScreenIntegration::default(),
            ui_signature,
        }
    }
}
