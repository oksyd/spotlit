use std::{
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use slint::{ComponentHandle, Weak};
use slint::{Model, ModelRc};

use crate::{
    MainWindow, WallpaperItem,
    bridge::{PreparedSnapshot, prepare_snapshot},
    command::Command,
    diagnostics::{self, Metric},
    image_cache,
    preview_image::{DecodedPreviewImage, decode_display_image, decode_thumbnail_image},
    worker::{CommandFailure, SettingsSnapshot, WorkerEvent},
};

const IMAGE_THREAD_STACK_SIZE: usize = 512 * 1024;
const LIST_THUMBNAIL_BATCH_SIZE: usize = 6;
const LIST_THUMBNAIL_BATCH_GAP: Duration = Duration::from_millis(16);
const LIST_THUMBNAIL_LIMIT: usize = image_cache::LIST_THUMBNAIL_CACHE_LIMIT;
const IMAGE_RESULT_RETRY_DELAY: Duration = Duration::from_millis(32);
const IMAGE_RESULT_RETRIES: u8 = 8;
const WORKER_EVENT_APPLY_RETRY_DELAY: Duration = Duration::from_millis(32);
const WORKER_EVENT_APPLY_RETRIES: u8 = 16;
const ACTION_FEEDBACK_DURATION: Duration = Duration::from_millis(2400);

static CURRENT_PREVIEW_LOADER: OnceLock<CurrentPreviewLoader> = OnceLock::new();
static LIST_THUMBNAIL_LOADER: OnceLock<ListThumbnailLoader> = OnceLock::new();
static WINDOW_ACCEPTS_IMAGE_WORK: AtomicBool = AtomicBool::new(true);
static ACTION_FEEDBACK_GENERATION: AtomicU64 = AtomicU64::new(0);
static SETTINGS_FEEDBACK_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Default)]
pub(crate) struct UiSink {
    current: Arc<Mutex<Option<Weak<MainWindow>>>>,
}

impl UiSink {
    pub(crate) fn set_current(&self, app: &MainWindow) {
        match self.current.lock() {
            Ok(mut current) => {
                *current = Some(app.as_weak());
            }
            Err(error) => {
                tracing::warn!(%error, "ui sink was poisoned while setting current window");
            }
        }
    }

    pub(crate) fn clear_current(&self) {
        match self.current.lock() {
            Ok(mut current) => {
                *current = None;
            }
            Err(error) => {
                tracing::warn!(%error, "ui sink was poisoned while clearing current window");
            }
        }
    }

    pub(crate) fn current(&self) -> Option<Weak<MainWindow>> {
        self.current.lock().ok().and_then(|current| current.clone())
    }
}

#[derive(Clone)]
struct ImageLoadRequest {
    id: String,
    path: PathBuf,
    path_text: String,
    row: Option<usize>,
}

#[derive(Clone)]
struct CurrentPreviewLoader {
    state: Arc<CurrentPreviewLoaderState>,
}

struct CurrentPreviewLoaderState {
    pending: Mutex<Option<CurrentPreviewLoad>>,
    active_request: Mutex<Option<CurrentPreviewRequestKey>>,
    changed: Condvar,
    generation: AtomicU64,
}

struct CurrentPreviewLoad {
    ui: Weak<MainWindow>,
    request: ImageLoadRequest,
    key: CurrentPreviewRequestKey,
    generation: u64,
}

#[derive(Clone, Eq, PartialEq)]
struct CurrentPreviewRequestKey {
    id: String,
    path_text: String,
}

#[derive(Clone)]
struct ListThumbnailLoader {
    state: Arc<ListThumbnailLoaderState>,
}

struct ListThumbnailLoaderState {
    pending: Mutex<Option<ListThumbnailLoad>>,
    active_request: Mutex<Option<ListThumbnailRequestKey>>,
    changed: Condvar,
    generation: AtomicU64,
}

struct ListThumbnailLoad {
    ui: Weak<MainWindow>,
    requests: Vec<ImageLoadRequest>,
    key: ListThumbnailRequestKey,
    generation: u64,
}

#[derive(Clone, Eq, PartialEq)]
struct ListThumbnailRequestKey {
    requests: Vec<ListThumbnailRequestKeyItem>,
}

#[derive(Clone, Eq, PartialEq)]
struct ListThumbnailRequestKeyItem {
    id: String,
    path_text: String,
    row: Option<usize>,
}

struct LoadedThumbnail {
    id: String,
    preview_path: String,
    row: Option<usize>,
    image: DecodedPreviewImage,
}

struct ReadyThumbnail {
    id: String,
    preview_path: String,
    row: Option<usize>,
    image: slint::Image,
}

struct ReadyCurrentPreview {
    id: String,
    source_path: String,
    preview_path: String,
    image: DecodedPreviewImage,
    thumbnail: Option<DecodedPreviewImage>,
    cache_display: bool,
    list_thumbnail_path: Option<String>,
}

struct ThumbnailWork {
    ready: Vec<ReadyThumbnail>,
    requests: Vec<ImageLoadRequest>,
}

struct ThumbnailDecodeGroup {
    path: PathBuf,
    requests: Vec<ImageLoadRequest>,
}

pub(crate) enum PreparedWorkerEvent {
    AutoSyncIdle,
    ConfigUpdated(String, SettingsSnapshot),
    OpenedPath(String),
    Snapshot(Arc<PreparedSnapshot>),
    SnapshotUnchanged,
    Synced(crate::core::SyncReport, Arc<PreparedSnapshot>),
    FavoriteUpdated(crate::core::FavoriteUpdate, Arc<PreparedSnapshot>),
    SettingsUpdated(String, Arc<PreparedSnapshot>),
    Failed(CommandFailure),
}

impl PreparedWorkerEvent {
    pub(crate) fn has_snapshot(&self) -> bool {
        matches!(
            self,
            Self::Snapshot(_)
                | Self::Synced(_, _)
                | Self::FavoriteUpdated(_, _)
                | Self::SettingsUpdated(_, _)
        )
    }

    fn language(&self) -> Option<crate::core::LanguageMode> {
        match self {
            Self::ConfigUpdated(_, settings) => Some(settings.config.language),
            Self::Snapshot(snapshot)
            | Self::Synced(_, snapshot)
            | Self::FavoriteUpdated(_, snapshot)
            | Self::SettingsUpdated(_, snapshot) => Some(snapshot.config.language),
            Self::AutoSyncIdle
            | Self::OpenedPath(_)
            | Self::SnapshotUnchanged
            | Self::Failed(_) => None,
        }
    }
}

impl CurrentPreviewLoader {
    fn start() -> Self {
        let state = Arc::new(CurrentPreviewLoaderState {
            pending: Mutex::new(None),
            active_request: Mutex::new(None),
            changed: Condvar::new(),
            generation: AtomicU64::new(0),
        });

        let loader_state = Arc::clone(&state);
        if let Err(error) = thread::Builder::new()
            .name("spotlit-current-preview-loader".to_string())
            .stack_size(IMAGE_THREAD_STACK_SIZE)
            .spawn(move || {
                run_current_preview_loader(loader_state);
            })
        {
            tracing::warn!(%error, "failed to spawn current preview loader");
        }

        Self { state }
    }

    fn queue(&self, ui: Weak<MainWindow>, request: ImageLoadRequest) {
        let key = CurrentPreviewRequestKey::from_request(&request);
        if self.remember_request(key.clone()) {
            return;
        }

        let generation = self.state.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let load = CurrentPreviewLoad {
            ui,
            request,
            key,
            generation,
        };

        match self.state.pending.lock() {
            Ok(mut pending) => {
                *pending = Some(load);
                self.state.changed.notify_one();
            }
            Err(error) => {
                tracing::warn!(%error, "current preview loader queue was poisoned");
            }
        }
    }

    fn remember_request(&self, key: CurrentPreviewRequestKey) -> bool {
        match self.state.active_request.lock() {
            Ok(mut active_request) => {
                if active_request.as_ref() == Some(&key) {
                    return true;
                }

                *active_request = Some(key);
                false
            }
            Err(error) => {
                tracing::warn!(%error, "current preview request cache was poisoned");
                false
            }
        }
    }

    fn cancel_pending(&self) {
        self.state.generation.fetch_add(1, Ordering::AcqRel);

        match self.state.pending.lock() {
            Ok(mut pending) => {
                *pending = None;
            }
            Err(error) => {
                tracing::warn!(%error, "current preview loader queue was poisoned");
            }
        }

        match self.state.active_request.lock() {
            Ok(mut active_request) => {
                *active_request = None;
            }
            Err(error) => {
                tracing::warn!(%error, "current preview request cache was poisoned");
            }
        }

        self.state.changed.notify_all();
    }
}

impl CurrentPreviewRequestKey {
    fn from_request(request: &ImageLoadRequest) -> Self {
        Self {
            id: request.id.clone(),
            path_text: request.path_text.clone(),
        }
    }
}

impl ListThumbnailLoader {
    fn start() -> Self {
        let state = Arc::new(ListThumbnailLoaderState {
            pending: Mutex::new(None),
            active_request: Mutex::new(None),
            changed: Condvar::new(),
            generation: AtomicU64::new(0),
        });

        let loader_state = Arc::clone(&state);
        if let Err(error) = thread::Builder::new()
            .name("spotlit-list-thumbnail-loader".to_string())
            .stack_size(IMAGE_THREAD_STACK_SIZE)
            .spawn(move || {
                crate::platform::enter_background_thread_mode();
                run_list_thumbnail_loader(loader_state);
            })
        {
            tracing::warn!(%error, "failed to spawn list thumbnail loader");
        }

        Self { state }
    }

    fn queue(&self, ui: Weak<MainWindow>, requests: Vec<ImageLoadRequest>) {
        if requests.is_empty() {
            return;
        }

        let key = ListThumbnailRequestKey::from_requests(&requests);
        if self.remember_request(key.clone()) {
            return;
        }

        let generation = self.state.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let load = ListThumbnailLoad {
            ui,
            requests,
            key,
            generation,
        };

        match self.state.pending.lock() {
            Ok(mut pending) => {
                *pending = Some(load);
                self.state.changed.notify_one();
            }
            Err(error) => {
                tracing::warn!(%error, "list thumbnail loader queue was poisoned");
            }
        }
    }

    fn remember_request(&self, key: ListThumbnailRequestKey) -> bool {
        match self.state.active_request.lock() {
            Ok(mut active_request) => {
                if active_request.as_ref() == Some(&key) {
                    return true;
                }

                *active_request = Some(key);
                false
            }
            Err(error) => {
                tracing::warn!(%error, "list thumbnail request cache was poisoned");
                false
            }
        }
    }

    fn cancel_pending(&self) {
        self.state.generation.fetch_add(1, Ordering::AcqRel);

        match self.state.pending.lock() {
            Ok(mut pending) => {
                *pending = None;
            }
            Err(error) => {
                tracing::warn!(%error, "list thumbnail loader queue was poisoned");
            }
        }

        match self.state.active_request.lock() {
            Ok(mut active_request) => {
                *active_request = None;
            }
            Err(error) => {
                tracing::warn!(%error, "list thumbnail request cache was poisoned");
            }
        }

        self.state.changed.notify_all();
    }
}

impl ListThumbnailRequestKey {
    fn from_requests(requests: &[ImageLoadRequest]) -> Self {
        Self {
            requests: requests
                .iter()
                .map(|request| ListThumbnailRequestKeyItem {
                    id: request.id.clone(),
                    path_text: request.path_text.clone(),
                    row: request.row,
                })
                .collect(),
        }
    }
}

pub(crate) fn prepare_worker_event(event: WorkerEvent) -> PreparedWorkerEvent {
    match event {
        WorkerEvent::AutoSyncIdle => PreparedWorkerEvent::AutoSyncIdle,
        WorkerEvent::ConfigUpdated(message, settings) => {
            PreparedWorkerEvent::ConfigUpdated(message, settings)
        }
        WorkerEvent::OpenedPath(message) => PreparedWorkerEvent::OpenedPath(message),
        WorkerEvent::Snapshot(snapshot) => {
            PreparedWorkerEvent::Snapshot(Arc::new(prepare_snapshot(snapshot)))
        }
        WorkerEvent::Synced(report, snapshot) => {
            PreparedWorkerEvent::Synced(report, Arc::new(prepare_snapshot(snapshot)))
        }
        WorkerEvent::FavoriteUpdated(update, snapshot) => {
            PreparedWorkerEvent::FavoriteUpdated(update, Arc::new(prepare_snapshot(snapshot)))
        }
        WorkerEvent::SettingsUpdated(message, snapshot) => {
            PreparedWorkerEvent::SettingsUpdated(message, Arc::new(prepare_snapshot(snapshot)))
        }
        WorkerEvent::Failed(failure) => PreparedWorkerEvent::Failed(failure),
    }
}

pub(crate) fn post_prepared_worker_event(ui: UiSink, event: PreparedWorkerEvent) {
    post_worker_event_with_retry(ui, event, WORKER_EVENT_APPLY_RETRIES);
}

fn post_worker_event_with_retry(ui: UiSink, event: PreparedWorkerEvent, retries: u8) {
    if let Err(error) = slint::invoke_from_event_loop(move || {
        if let Some(language) = event.language() {
            crate::i18n::select_language(language);
        }

        let Some(app) = ui.current().and_then(|ui| ui.upgrade()) else {
            return;
        };

        if should_defer_worker_event(&app, &event, retries) {
            if retries > 0 {
                slint::Timer::single_shot(WORKER_EVENT_APPLY_RETRY_DELAY, move || {
                    post_worker_event_with_retry(ui, event, retries - 1);
                });
            }
            return;
        }

        apply_worker_event(&app, event);
    }) {
        tracing::warn!(%error, "failed to queue worker event on UI event loop");
    }
}

fn should_defer_worker_event(app: &MainWindow, event: &PreparedWorkerEvent, retries: u8) -> bool {
    should_defer_snapshot_event(
        event.has_snapshot(),
        window_accepts_image_work(app),
        retries,
    )
}

fn should_defer_snapshot_event(
    has_snapshot: bool,
    window_accepts_image_work: bool,
    retries: u8,
) -> bool {
    has_snapshot && !window_accepts_image_work && retries > 0
}

fn apply_worker_event(app: &MainWindow, event: PreparedWorkerEvent) {
    let has_snapshot = event.has_snapshot();
    match event {
        PreparedWorkerEvent::AutoSyncIdle => {}
        PreparedWorkerEvent::ConfigUpdated(message, settings) => {
            tracing::debug!(%message, "worker updated configuration");
            crate::bridge::apply_settings(app, settings);
            app.set_settings_save_pending(false);
            show_config_updated_feedback(app, message);
        }
        PreparedWorkerEvent::OpenedPath(message) => {
            tracing::debug!(%message, "worker opened path");
            clear_import_pending_after_opened_path(app, &message);
            clear_external_action_after_opened_path(app, &message);
            if should_show_opened_path_feedback(&message) {
                show_opened_path_feedback(app, message);
            }
        }
        PreparedWorkerEvent::Failed(failure) => {
            tracing::warn!(
                command = failure.command.name(),
                message = %failure.message,
                "worker command failed"
            );
            apply_command_failure(app, failure);
        }
        PreparedWorkerEvent::Snapshot(snapshot) => {
            let refresh_pending = app.get_refresh_pending();
            crate::bridge::apply_prepared_snapshot(app, &snapshot);
            app.set_refresh_pending(false);
            if refresh_pending {
                show_action_feedback(app, "Library refreshed");
            }
        }
        PreparedWorkerEvent::SnapshotUnchanged => {
            if app.get_refresh_pending() {
                app.set_refresh_pending(false);
                show_action_feedback(app, "Library up to date");
            }
        }
        PreparedWorkerEvent::Synced(report, snapshot) => {
            tracing::debug!(
                id = %report.id,
                image_path = %report.image_path.display(),
                synced_at = %report.synced_at,
                "worker applied wallpaper"
            );
            crate::bridge::apply_prepared_snapshot(app, &snapshot);
            app.set_sync_pending(false);
            app.set_sync_pending_id("".into());
            show_action_feedback(app, "Wallpaper applied");
        }
        PreparedWorkerEvent::FavoriteUpdated(update, snapshot) => {
            let feedback = favorite_feedback(update.favorite);
            tracing::debug!(
                id = %update.id,
                favorite = update.favorite,
                "worker updated favorite"
            );
            crate::bridge::apply_prepared_snapshot(app, &snapshot);
            clear_favorite_pending(app, update.id.as_str());
            show_action_feedback(app, feedback);
        }
        PreparedWorkerEvent::SettingsUpdated(message, snapshot) => {
            tracing::debug!(%message, "worker updated settings");
            crate::bridge::apply_prepared_snapshot(app, &snapshot);
            clear_import_pending_after_settings_update(app, &message);
            clear_clean_cache_pending_after_settings_update(app, &message);
            clear_removing_wallpaper_after_settings_update(app, &message);
            clear_settings_save_pending_after_settings_update(app, &message);
            show_settings_update_feedback(app, &message);
        }
    }

    if has_snapshot {
        diagnostics::mark_since_start(Metric::FirstSnapshot);
    }

    if has_snapshot && window_accepts_image_work(app) {
        request_snapshot_images(app, current_preview_request_from_ui(app));
        request_list_thumbnails(app);
    }

    diagnostics::apply_to_ui(app);
}

pub(crate) fn show_action_feedback(app: &MainWindow, message: impl Into<slint::SharedString>) {
    let message = message.into();
    let message = crate::i18n::message(app, message.as_str());
    let generation = ACTION_FEEDBACK_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    app.set_action_feedback_text(message.clone());

    let ui = app.as_weak();
    slint::Timer::single_shot(ACTION_FEEDBACK_DURATION, move || {
        if ACTION_FEEDBACK_GENERATION.load(Ordering::Acquire) != generation {
            return;
        }

        let Some(app) = ui.upgrade() else {
            return;
        };

        if app.get_action_feedback_text() == message {
            app.set_action_feedback_text("".into());
        }
    });
}

pub(crate) fn set_action_feedback_status(
    app: &MainWindow,
    message: impl Into<slint::SharedString>,
) {
    ACTION_FEEDBACK_GENERATION.fetch_add(1, Ordering::AcqRel);
    let message = message.into();
    app.set_action_feedback_text(crate::i18n::message(app, message.as_str()));
}

pub(crate) fn clear_action_feedback(app: &MainWindow) {
    ACTION_FEEDBACK_GENERATION.fetch_add(1, Ordering::AcqRel);
    app.set_action_feedback_text("".into());
}

pub(crate) fn show_settings_feedback(app: &MainWindow, message: impl Into<slint::SharedString>) {
    if !app.get_show_settings() {
        return;
    }

    let message = message.into();
    let message = crate::i18n::message(app, message.as_str());
    let generation = SETTINGS_FEEDBACK_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    app.set_settings_feedback_text(message.clone());

    let ui = app.as_weak();
    slint::Timer::single_shot(ACTION_FEEDBACK_DURATION, move || {
        if SETTINGS_FEEDBACK_GENERATION.load(Ordering::Acquire) != generation {
            return;
        }

        let Some(app) = ui.upgrade() else {
            return;
        };

        if app.get_settings_feedback_text() == message {
            app.set_settings_feedback_text("".into());
        }
    });
}

pub(crate) fn set_settings_feedback_status(
    app: &MainWindow,
    message: impl Into<slint::SharedString>,
) {
    SETTINGS_FEEDBACK_GENERATION.fetch_add(1, Ordering::AcqRel);
    let message = message.into();
    app.set_settings_feedback_text(crate::i18n::message(app, message.as_str()));
}

pub(crate) fn clear_settings_feedback(app: &MainWindow) {
    SETTINGS_FEEDBACK_GENERATION.fetch_add(1, Ordering::AcqRel);
    app.set_settings_feedback_text("".into());
}

fn show_settings_update_feedback(app: &MainWindow, message: &str) {
    if is_quiet_settings_update(message) {
        return;
    }

    if app.get_show_settings() && is_settings_context_update(message) {
        show_settings_feedback(app, message.to_string());
        return;
    }

    clear_hidden_settings_feedback(app);
    show_action_feedback(app, message.to_string());
    show_settings_feedback(app, message.to_string());
}

fn show_config_updated_feedback(app: &MainWindow, message: String) {
    show_settings_operation_feedback(app, message);
}

fn show_opened_path_feedback(app: &MainWindow, message: String) {
    if is_settings_opened_path_message(&message) {
        show_settings_operation_feedback(app, message);
    } else {
        show_action_feedback(app, message);
    }
}

pub(crate) fn show_settings_operation_feedback(
    app: &MainWindow,
    message: impl Into<slint::SharedString>,
) {
    let message = message.into();
    if app.get_show_settings() {
        show_settings_feedback(app, message);
    } else {
        clear_hidden_settings_feedback(app);
        show_action_feedback(app, message);
    }
}

fn is_quiet_settings_update(message: &str) -> bool {
    message == "Preview cache updated"
}

fn clear_hidden_settings_feedback(app: &MainWindow) {
    if !app.get_show_settings() {
        clear_settings_feedback(app);
    }
}

fn is_settings_context_update(message: &str) -> bool {
    message.starts_with("Cache cleaned:") || message.starts_with("History limit saved")
}

fn clear_import_pending_after_opened_path(app: &MainWindow, message: &str) {
    if message == "Import canceled" {
        app.set_import_pending(false);
        clear_matching_action_feedback(app, "Opening import dialog");
    }
}

fn clear_external_action_after_opened_path(app: &MainWindow, message: &str) {
    app.set_external_action_pending(false);
    app.set_external_action_key("".into());
    app.set_external_action_id("".into());

    if message == "Export canceled" {
        clear_matching_action_feedback(app, "Opening export dialog");
    }
}

fn should_show_opened_path_feedback(message: &str) -> bool {
    message != "Import canceled" && message != "Export canceled"
}

fn is_settings_opened_path_message(message: &str) -> bool {
    matches!(
        message,
        "Opened data folder"
            | "Opened favorites folder"
            | "Opened logs folder"
            | "Opened Spotlit release page"
    )
}

fn clear_import_pending_after_settings_update(app: &MainWindow, message: &str) {
    if is_import_result_message(message) {
        app.set_import_pending(false);
    }
}

fn clear_clean_cache_pending_after_settings_update(app: &MainWindow, message: &str) {
    if message.starts_with("Cache cleaned:") {
        app.set_clean_cache_pending(false);
    }
}

fn clear_removing_wallpaper_after_settings_update(app: &MainWindow, message: &str) {
    let Some(id) = message.strip_prefix("Removed ") else {
        return;
    };

    clear_removing_wallpaper(app, id);
}

fn clear_settings_save_pending_after_settings_update(app: &MainWindow, message: &str) {
    if is_settings_save_result_message(message) {
        app.set_settings_save_pending(false);
    }
}

fn is_import_result_message(message: &str) -> bool {
    message.starts_with("Imported ")
        || message == "Selected file is not a supported landscape wallpaper"
}

fn is_settings_save_result_message(message: &str) -> bool {
    matches!(
        message,
        "Settings saved"
            | "Wallpaper source saved"
            | "Sync interval saved"
            | "Startup setting saved"
            | "Background setting saved"
            | "Update setting saved"
            | "Language setting saved"
            | "Theme setting saved"
            | "Auto sync enabled and synced"
            | "GNOME extension installed"
            | "GNOME extension enabled"
            | "GNOME extension disabled"
            | "Lock screen blur saved"
            | "Lock screen display saved"
    ) || message.starts_with("History limit saved")
}

fn apply_command_failure(app: &MainWindow, failure: CommandFailure) {
    match &failure.command {
        Command::ImportImage => app.set_import_pending(false),
        Command::InstallLockScreenIntegration
        | Command::SetLockScreenBlurMode(_)
        | Command::SetLockScreenDisplayMode(_)
        | Command::SetLockScreenIntegrationEnabled(_) => app.set_settings_save_pending(false),
        Command::Scan => app.set_refresh_pending(false),
        Command::SyncCurrent | Command::SyncWallpaper { .. } => {
            app.set_sync_pending(false);
            app.set_sync_pending_id("".into());
        }
        Command::CleanCache => app.set_clean_cache_pending(false),
        Command::SetFavorite { id, favorite } => {
            clear_favorite_pending(app, id);
            crate::bridge::apply_favorite_optimistic(app, id, !*favorite);
        }
        Command::RemoveWallpaper { id } => clear_removing_wallpaper(app, id),
        Command::ExportWallpaper { .. }
        | Command::OpenDataFolder
        | Command::OpenFavoritesFolder
        | Command::OpenLogsFolder
        | Command::OpenReleasePage
        | Command::OpenWallpaperInfo { .. }
        | Command::RevealCurrentImage
        | Command::RevealWallpaper { .. } => {
            app.set_external_action_pending(false);
            app.set_external_action_key("".into());
            app.set_external_action_id("".into());
        }
        Command::SetAutoSync(_)
        | Command::SetAutomaticUpdateChecks(_)
        | Command::SetHistoryLimit(_)
        | Command::SetKeepRunningInBackground(_)
        | Command::SetLanguage(_)
        | Command::SetWallpaperSource(_)
        | Command::SetStartAtLogin(_)
        | Command::SetSyncInterval(_)
        | Command::SetTheme(_) => app.set_settings_save_pending(false),
        Command::AutoSyncTick | Command::LoadSnapshot | Command::WarmThumbnails => {}
    }

    show_command_failure_feedback(app, &failure.command, &failure.message);
}

fn clear_matching_action_feedback(app: &MainWindow, message: &str) {
    if app.get_action_feedback_text() == crate::i18n::message(app, message) {
        clear_action_feedback(app);
    }
}

fn clear_removing_wallpaper(app: &MainWindow, id: &str) {
    if app.get_removing_wallpaper_id().as_str() == id {
        app.set_removing_wallpaper_id("".into());
    }
}

fn clear_favorite_pending(app: &MainWindow, id: &str) {
    if app.get_favorite_pending_id().as_str() == id {
        app.set_favorite_pending_id("".into());
    }
}

fn failure_feedback(message: &str) -> String {
    format!("Failed: {message}")
}

fn show_command_failure_feedback(app: &MainWindow, command: &Command, message: &str) {
    let feedback = failure_feedback(message);
    if app.get_show_settings() && command_belongs_to_settings_context(command) {
        show_settings_feedback(app, feedback);
        return;
    }

    clear_hidden_settings_feedback(app);
    show_action_feedback(app, feedback.clone());
    show_settings_feedback(app, feedback);
}

fn command_belongs_to_settings_context(command: &Command) -> bool {
    matches!(
        command,
        Command::CleanCache
            | Command::OpenDataFolder
            | Command::OpenFavoritesFolder
            | Command::OpenLogsFolder
            | Command::OpenReleasePage
            | Command::SetAutoSync(_)
            | Command::SetAutomaticUpdateChecks(_)
            | Command::InstallLockScreenIntegration
            | Command::SetHistoryLimit(_)
            | Command::SetKeepRunningInBackground(_)
            | Command::SetLanguage(_)
            | Command::SetLockScreenBlurMode(_)
            | Command::SetLockScreenDisplayMode(_)
            | Command::SetLockScreenIntegrationEnabled(_)
            | Command::SetWallpaperSource(_)
            | Command::SetStartAtLogin(_)
            | Command::SetSyncInterval(_)
            | Command::SetTheme(_)
    )
}

fn favorite_feedback(favorite: bool) -> &'static str {
    if favorite {
        "Added to favorites"
    } else {
        "Removed from favorites"
    }
}

pub(crate) fn request_visible_images(app: &MainWindow) {
    if !window_accepts_image_work(app) {
        return;
    }

    request_snapshot_images(app, current_preview_request_from_ui(app));
    request_list_thumbnails(app);
}

fn request_snapshot_images(app: &MainWindow, current_preview: Option<ImageLoadRequest>) {
    if let Some(request) = current_preview
        && app.get_current_preview_path().as_str() != request.path_text
    {
        spawn_current_preview_loader(app.as_weak(), request);
    }
}

pub(crate) fn set_window_accepts_image_work(accepts: bool) {
    WINDOW_ACCEPTS_IMAGE_WORK.store(accepts, Ordering::Release);
}

pub(crate) fn suspend_image_work() {
    set_window_accepts_image_work(false);
    if let Some(loader) = CURRENT_PREVIEW_LOADER.get() {
        loader.cancel_pending();
    }
    if let Some(loader) = LIST_THUMBNAIL_LOADER.get() {
        loader.cancel_pending();
    }
    image_cache::release_decoded_images();
}

pub(crate) fn window_accepts_image_work(app: &MainWindow) -> bool {
    WINDOW_ACCEPTS_IMAGE_WORK.load(Ordering::Acquire)
        && app.window().is_visible()
        && !app.window().is_minimized()
}

fn current_preview_request_from_ui(app: &MainWindow) -> Option<ImageLoadRequest> {
    if !app.get_has_current() {
        return None;
    }

    image_load_request_from_text(
        app.get_current_id().to_string(),
        app.get_current_preview_source_path().as_str(),
    )
}

fn image_load_request_from_text(id: String, path: &str) -> Option<ImageLoadRequest> {
    if id.is_empty() || path.is_empty() {
        return None;
    }

    Some(ImageLoadRequest {
        id,
        path: PathBuf::from(path),
        path_text: path.to_string(),
        row: None,
    })
}

fn spawn_current_preview_loader(ui: Weak<MainWindow>, request: ImageLoadRequest) {
    current_preview_loader().queue(ui, request);
}

fn current_preview_loader() -> &'static CurrentPreviewLoader {
    CURRENT_PREVIEW_LOADER.get_or_init(CurrentPreviewLoader::start)
}

fn request_list_thumbnails(app: &MainWindow) {
    let work = if app.get_favorites_only_enabled() {
        list_thumbnail_work(&app.get_favorite_wallpapers())
    } else {
        list_thumbnail_work(&app.get_wallpapers())
    };

    apply_ready_thumbnail_batch(app, work.ready);
    if !work.requests.is_empty() {
        list_thumbnail_loader().queue(app.as_weak(), work.requests);
    }
}

fn list_thumbnail_loader() -> &'static ListThumbnailLoader {
    LIST_THUMBNAIL_LOADER.get_or_init(ListThumbnailLoader::start)
}

fn list_thumbnail_work(model: &ModelRc<WallpaperItem>) -> ThumbnailWork {
    let row_count = model.row_count().min(LIST_THUMBNAIL_LIMIT);
    let mut ready = Vec::new();
    let mut requests = Vec::new();

    for row in 0..row_count {
        let Some(item) = model.row_data(row) else {
            continue;
        };

        if item.thumbnail_ready || item.preview_path.is_empty() {
            continue;
        }

        if let Some(image) =
            image_cache::list_thumbnail(item.id.as_str(), item.preview_path.as_str())
                .or_else(|| image_cache::list_thumbnail_by_path(item.preview_path.as_str()))
        {
            ready.push(ReadyThumbnail {
                id: item.id.to_string(),
                preview_path: item.preview_path.to_string(),
                row: Some(row),
                image,
            });
            continue;
        }

        requests.push(image_load_request_from_item(&item, row));
    }

    ThumbnailWork { ready, requests }
}

fn image_load_request_from_item(item: &WallpaperItem, row: usize) -> ImageLoadRequest {
    ImageLoadRequest {
        id: item.id.to_string(),
        path: PathBuf::from(item.preview_path.as_str()),
        path_text: item.preview_path.to_string(),
        row: Some(row),
    }
}

fn run_current_preview_loader(state: Arc<CurrentPreviewLoaderState>) {
    loop {
        let mut pending = match state.pending.lock() {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "current preview loader queue was poisoned");
                return;
            }
        };

        while pending.is_none() {
            pending = match state.changed.wait(pending) {
                Ok(pending) => pending,
                Err(error) => {
                    tracing::warn!(%error, "current preview loader wait failed");
                    return;
                }
            };
        }

        let Some(load) = pending.take() else {
            continue;
        };
        drop(pending);

        if state.generation.load(Ordering::Acquire) != load.generation {
            clear_current_preview_request(&state, &load.key);
            continue;
        }

        let started_at = Instant::now();
        let Some(image) = decode_display_image(&load.request.path) else {
            tracing::warn!(
                id = %load.request.id,
                path = %load.request.path.display(),
                "failed to decode current preview"
            );
            clear_current_preview_request(&state, &load.key);
            continue;
        };
        let thumbnail = image.thumbnail_copy();
        diagnostics::record(Metric::CurrentPreview, started_at.elapsed());
        if state.generation.load(Ordering::Acquire) != load.generation {
            clear_current_preview_request(&state, &load.key);
            continue;
        }
        let list_thumbnail_path = load.request.path_text.clone();
        post_current_preview(
            Arc::clone(&state),
            load.ui,
            load.generation,
            load.key,
            ReadyCurrentPreview {
                id: load.request.id,
                source_path: load.request.path_text.clone(),
                preview_path: load.request.path_text,
                image,
                thumbnail,
                cache_display: true,
                list_thumbnail_path: Some(list_thumbnail_path),
            },
            IMAGE_RESULT_RETRIES,
        );
    }
}

fn post_current_preview(
    loader_state: Arc<CurrentPreviewLoaderState>,
    ui: Weak<MainWindow>,
    generation: u64,
    key: CurrentPreviewRequestKey,
    ready: ReadyCurrentPreview,
    retries: u8,
) {
    if let Err(error) = slint::invoke_from_event_loop(move || {
        if loader_state.generation.load(Ordering::Acquire) != generation {
            clear_current_preview_request(&loader_state, &key);
            return;
        }

        let Some(app) = ui.upgrade() else {
            clear_current_preview_request(&loader_state, &key);
            return;
        };
        if !app.window().is_visible() || app.window().is_minimized() {
            clear_current_preview_request(&loader_state, &key);
            return;
        }
        if !window_accepts_image_work(&app) {
            if retries > 0 {
                slint::Timer::single_shot(IMAGE_RESULT_RETRY_DELAY, move || {
                    post_current_preview(loader_state, ui, generation, key, ready, retries - 1);
                });
            } else {
                clear_current_preview_request(&loader_state, &key);
            }
            return;
        }

        let ReadyCurrentPreview {
            id,
            source_path,
            preview_path,
            image,
            thumbnail,
            cache_display,
            list_thumbnail_path,
        } = ready;

        if !app.get_has_current()
            || app.get_current_id().as_str() != id
            || app.get_current_preview_source_path().as_str() != source_path
        {
            clear_current_preview_request(&loader_state, &key);
            return;
        }

        let image = image.into_slint_image();
        if cache_display {
            image_cache::remember_display_preview(&id, &preview_path, &image);
        }
        crate::bridge::apply_current_preview(&app, image.clone(), preview_path.into());

        if let (Some(thumbnail), Some(list_thumbnail_path)) = (thumbnail, list_thumbnail_path) {
            let thumbnail = thumbnail.into_slint_image();
            image_cache::remember_list_thumbnail(&id, &list_thumbnail_path, &thumbnail);
            apply_ready_thumbnail(
                &app,
                ReadyThumbnail {
                    id: id.clone(),
                    preview_path: list_thumbnail_path,
                    row: None,
                    image: thumbnail,
                },
            );
        }

        let selected = app.get_selected_wallpaper();
        if app.get_has_selection()
            && selected.id.as_str() == id
            && selected_matches_preview_source(&selected, &source_path)
        {
            crate::bridge::apply_selected_preview(&app, image, source_path.into());
        }

        diagnostics::apply_to_ui(&app);
        clear_current_preview_request(&loader_state, &key);
    }) {
        tracing::warn!(%error, "failed to queue current preview on UI event loop");
    }
}

fn clear_current_preview_request(
    state: &CurrentPreviewLoaderState,
    key: &CurrentPreviewRequestKey,
) {
    match state.active_request.lock() {
        Ok(mut active_request) => {
            if active_request.as_ref() == Some(key) {
                *active_request = None;
            }
        }
        Err(error) => {
            tracing::warn!(%error, "current preview request cache was poisoned");
        }
    }
}

fn run_list_thumbnail_loader(state: Arc<ListThumbnailLoaderState>) {
    loop {
        let mut pending = match state.pending.lock() {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "list thumbnail loader queue was poisoned");
                return;
            }
        };

        while pending.is_none() {
            pending = match state.changed.wait(pending) {
                Ok(pending) => pending,
                Err(error) => {
                    tracing::warn!(%error, "list thumbnail loader wait failed");
                    return;
                }
            };
        }

        let Some(load) = pending.take() else {
            continue;
        };
        drop(pending);

        if state.generation.load(Ordering::Acquire) != load.generation {
            clear_active_list_thumbnail_request(&state, &load.key);
            continue;
        }

        let started_at = Instant::now();
        let mut chunks = load.requests.chunks(LIST_THUMBNAIL_BATCH_SIZE).peekable();
        while let Some(chunk) = chunks.next() {
            if state.generation.load(Ordering::Acquire) != load.generation {
                break;
            }

            let thumbnails = thumbnail_decode_groups(chunk)
                .into_iter()
                .filter_map(|group| {
                    let image = decode_thumbnail_image(&group.path)?;
                    Some(
                        group
                            .requests
                            .into_iter()
                            .map(|request| LoadedThumbnail {
                                id: request.id,
                                preview_path: request.path_text,
                                row: request.row,
                                image: image.clone(),
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .flatten()
                .collect::<Vec<_>>();

            if thumbnails.is_empty() {
                continue;
            }

            if state.generation.load(Ordering::Acquire) != load.generation {
                break;
            }

            let loader_state = Arc::clone(&state);
            let ui = load.ui.clone();
            let generation = load.generation;
            if let Err(error) = slint::invoke_from_event_loop(move || {
                if loader_state.generation.load(Ordering::Acquire) != generation {
                    return;
                }

                let Some(app) = ui.upgrade() else {
                    return;
                };
                if !window_accepts_image_work(&app) {
                    return;
                }

                apply_thumbnail_batch(&app, thumbnails);
            }) {
                tracing::warn!(%error, "failed to queue list thumbnails on UI event loop");
            }

            if chunks.peek().is_some()
                && state.generation.load(Ordering::Acquire) == load.generation
            {
                thread::sleep(LIST_THUMBNAIL_BATCH_GAP);
            }
        }

        diagnostics::record(Metric::Thumbnails, started_at.elapsed());
        post_thumbnail_diagnostics(state.clone(), load.ui.clone(), load.generation);
        clear_active_list_thumbnail_request(&state, &load.key);
    }
}

fn thumbnail_decode_groups(requests: &[ImageLoadRequest]) -> Vec<ThumbnailDecodeGroup> {
    let mut groups: Vec<ThumbnailDecodeGroup> = Vec::new();

    for request in requests {
        if let Some(group) = groups.iter_mut().find(|group| group.path == request.path) {
            group.requests.push(request.clone());
            continue;
        }

        groups.push(ThumbnailDecodeGroup {
            path: request.path.clone(),
            requests: vec![request.clone()],
        });
    }

    groups
}

fn post_thumbnail_diagnostics(
    loader_state: Arc<ListThumbnailLoaderState>,
    ui: Weak<MainWindow>,
    generation: u64,
) {
    if let Err(error) = slint::invoke_from_event_loop(move || {
        if loader_state.generation.load(Ordering::Acquire) != generation {
            return;
        }

        let Some(app) = ui.upgrade() else {
            return;
        };

        diagnostics::apply_to_ui(&app);
    }) {
        tracing::warn!(%error, "failed to queue thumbnail diagnostics on UI event loop");
    }
}

fn clear_active_list_thumbnail_request(
    state: &ListThumbnailLoaderState,
    key: &ListThumbnailRequestKey,
) {
    match state.active_request.lock() {
        Ok(mut active_request) => {
            if active_request.as_ref() == Some(key) {
                *active_request = None;
            }
        }
        Err(error) => {
            tracing::warn!(%error, "list thumbnail request cache was poisoned");
        }
    }
}

fn apply_thumbnail_batch(app: &MainWindow, thumbnails: Vec<LoadedThumbnail>) {
    for thumbnail in thumbnails {
        let LoadedThumbnail {
            id,
            preview_path,
            row,
            image,
        } = thumbnail;
        let image = image.into_slint_image();
        image_cache::remember_list_thumbnail(&id, &preview_path, &image);
        apply_ready_thumbnail(
            app,
            ReadyThumbnail {
                id,
                preview_path,
                row,
                image,
            },
        );
    }
}

fn apply_ready_thumbnail_batch(app: &MainWindow, thumbnails: Vec<ReadyThumbnail>) {
    for thumbnail in thumbnails {
        apply_ready_thumbnail(app, thumbnail);
    }
}

fn apply_ready_thumbnail(app: &MainWindow, thumbnail: ReadyThumbnail) {
    let ReadyThumbnail {
        id,
        preview_path,
        row,
        image,
    } = thumbnail;
    let thumbnail = LoadedThumbnailRef {
        id: &id,
        preview_path: &preview_path,
        row,
    };
    let visible_model = if app.get_favorites_only_enabled() {
        app.get_favorite_wallpapers()
    } else {
        app.get_wallpapers()
    };
    update_visible_model_thumbnail(&visible_model, thumbnail, &image);
    update_selected_thumbnail(app, thumbnail, &image);
    update_current_thumbnail_placeholder(app, thumbnail, &image);
}

#[derive(Clone, Copy)]
struct LoadedThumbnailRef<'a> {
    id: &'a str,
    preview_path: &'a str,
    row: Option<usize>,
}

fn update_visible_model_thumbnail(
    model: &ModelRc<WallpaperItem>,
    thumbnail: LoadedThumbnailRef<'_>,
    image: &slint::Image,
) {
    if let Some(row) = thumbnail.row
        && update_model_thumbnail_row(model, row, thumbnail, image)
    {
        return;
    }

    for row in 0..model.row_count().min(LIST_THUMBNAIL_LIMIT) {
        if update_model_thumbnail_row(model, row, thumbnail, image) {
            return;
        }
    }
}

fn update_model_thumbnail_row(
    model: &ModelRc<WallpaperItem>,
    row: usize,
    thumbnail: LoadedThumbnailRef<'_>,
    image: &slint::Image,
) -> bool {
    let Some(mut item) = model.row_data(row) else {
        return false;
    };

    if update_item_thumbnail(&mut item, thumbnail, image) {
        model.set_row_data(row, item);
        return true;
    }

    false
}

fn update_selected_thumbnail(
    app: &MainWindow,
    thumbnail: LoadedThumbnailRef<'_>,
    image: &slint::Image,
) {
    if !app.get_has_selection() {
        return;
    }

    let mut selected = app.get_selected_wallpaper();
    if update_item_thumbnail(&mut selected, thumbnail, image) {
        app.set_selected_wallpaper(selected);
    }
}

fn update_current_thumbnail_placeholder(
    app: &MainWindow,
    thumbnail: LoadedThumbnailRef<'_>,
    image: &slint::Image,
) {
    if !app.get_has_current()
        || app.get_current_id().as_str() != thumbnail.id
        || thumbnail.preview_path.is_empty()
    {
        return;
    }

    if app.get_current_preview_ready()
        && app.get_current_preview_path().as_str() == app.get_current_preview_source_path().as_str()
    {
        return;
    }

    let display_request = current_preview_request_from_ui(app);
    crate::bridge::apply_current_preview(app, image.clone(), thumbnail.preview_path.into());
    if let Some(request) = display_request {
        spawn_current_preview_loader(app.as_weak(), request);
    }
}

fn update_item_thumbnail(
    item: &mut WallpaperItem,
    thumbnail: LoadedThumbnailRef<'_>,
    image: &slint::Image,
) -> bool {
    if item.id.as_str() != thumbnail.id || item.preview_path.as_str() != thumbnail.preview_path {
        return false;
    }

    if item.thumbnail_ready && image_has_pixels(&item.thumbnail) {
        return false;
    }

    item.thumbnail = image.clone();
    item.thumbnail_ready = true;
    true
}

fn image_has_pixels(image: &slint::Image) -> bool {
    let size = image.size();
    size.width > 0 && size.height > 0
}

fn selected_matches_preview_source(item: &WallpaperItem, path: &str) -> bool {
    item.preview_path.as_str() == path || item.image_path.as_str() == path
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{Arc, Condvar, Mutex, atomic::AtomicU64},
    };

    use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

    use crate::core::AppConfig;

    use crate::{
        image_cache,
        platform::{LockScreenIntegration, SystemTheme},
        worker::{Snapshot, WorkerEvent},
    };

    use super::{
        CurrentPreviewLoader, CurrentPreviewLoaderState, ImageLoadRequest, ListThumbnailLoader,
        ListThumbnailLoaderState, LoadedThumbnailRef, PreparedWorkerEvent, WallpaperItem,
        clear_active_list_thumbnail_request, clear_current_preview_request,
        is_settings_save_result_message, list_thumbnail_work, prepare_worker_event,
        should_defer_snapshot_event, thumbnail_decode_groups, update_item_thumbnail,
        update_visible_model_thumbnail,
    };

    #[test]
    fn thumbnail_update_fills_missing_thumbnail() {
        let mut item = wallpaper_item("id");
        let image = test_image();

        let updated = update_item_thumbnail(
            &mut item,
            LoadedThumbnailRef {
                id: "id",
                preview_path: "preview.jpg",
                row: None,
            },
            &image,
        );

        assert!(updated);
        assert!(item.thumbnail_ready);
        assert!(item.thumbnail.size().width > 0);
    }

    #[test]
    fn thumbnail_update_skips_existing_thumbnail() {
        let image = test_image();
        let mut item = wallpaper_item("id");
        item.thumbnail = image.clone();
        item.thumbnail_ready = true;

        let updated = update_item_thumbnail(
            &mut item,
            LoadedThumbnailRef {
                id: "id",
                preview_path: "preview.jpg",
                row: None,
            },
            &image,
        );

        assert!(!updated);
    }

    #[test]
    fn visible_thumbnail_update_prefers_requested_row() {
        let image = test_image();
        let model = ModelRc::new(VecModel::from(vec![
            wallpaper_item_with_preview("id", "preview.jpg"),
            wallpaper_item_with_preview("id", "preview.jpg"),
        ]));

        update_visible_model_thumbnail(
            &model,
            LoadedThumbnailRef {
                id: "id",
                preview_path: "preview.jpg",
                row: Some(1),
            },
            &image,
        );

        assert!(!model.row_data(0).unwrap().thumbnail_ready);
        assert!(model.row_data(1).unwrap().thumbnail_ready);
    }

    #[test]
    fn all_settings_save_messages_clear_pending_state() {
        for message in [
            "Settings saved",
            "Wallpaper source saved",
            "Sync interval saved",
            "Startup setting saved",
            "Background setting saved",
            "Update setting saved",
            "Language setting saved",
            "Theme setting saved",
            "Auto sync enabled and synced",
            "History limit saved",
            "History limit saved: 1 wallpapers removed",
        ] {
            assert!(
                is_settings_save_result_message(message),
                "{message} should clear settings-save-pending"
            );
        }
    }

    #[test]
    fn cached_thumbnail_is_returned_without_decode_request() {
        let image = test_image();
        image_cache::remember_list_thumbnail("cached-id", "cached-preview.jpg", &image);
        let item = wallpaper_item_with_preview("cached-id", "cached-preview.jpg");
        let model = ModelRc::new(VecModel::from(vec![item]));

        let work = list_thumbnail_work(&model);

        assert_eq!(work.ready.len(), 1);
        assert!(work.requests.is_empty());
    }

    #[test]
    fn missing_thumbnail_is_queued_for_decode() {
        let item = wallpaper_item_with_preview("missing-id", "missing-preview.jpg");
        let model = ModelRc::new(VecModel::from(vec![item]));

        let work = list_thumbnail_work(&model);

        assert!(work.ready.is_empty());
        assert_eq!(work.requests.len(), 1);
    }

    #[test]
    fn duplicate_thumbnail_request_is_suppressed_while_active() {
        let loader = test_list_thumbnail_loader();
        let key = list_thumbnail_request_key("id", "preview.jpg");

        assert!(!loader.remember_request(key.clone()));
        assert!(loader.remember_request(key));
    }

    #[test]
    fn clearing_thumbnail_request_allows_same_request_again() {
        let loader = test_list_thumbnail_loader();
        let key = list_thumbnail_request_key("id", "preview.jpg");

        assert!(!loader.remember_request(key.clone()));
        clear_active_list_thumbnail_request(&loader.state, &key);

        assert!(!loader.remember_request(key));
    }

    #[test]
    fn canceling_thumbnail_loader_allows_same_request_again() {
        let loader = test_list_thumbnail_loader();
        let key = list_thumbnail_request_key("id", "preview.jpg");

        assert!(!loader.remember_request(key.clone()));
        loader.cancel_pending();

        assert!(!loader.remember_request(key));
    }

    #[test]
    fn duplicate_current_preview_request_is_suppressed_while_active() {
        let loader = test_current_preview_loader();
        let key = current_preview_request_key("id", "preview.jpg");

        assert!(!loader.remember_request(key.clone()));
        assert!(loader.remember_request(key));
    }

    #[test]
    fn clearing_current_preview_request_allows_same_request_again() {
        let loader = test_current_preview_loader();
        let key = current_preview_request_key("id", "preview.jpg");

        assert!(!loader.remember_request(key.clone()));
        clear_current_preview_request(&loader.state, &key);

        assert!(!loader.remember_request(key));
    }

    #[test]
    fn canceling_current_preview_loader_allows_same_request_again() {
        let loader = test_current_preview_loader();
        let key = current_preview_request_key("id", "preview.jpg");

        assert!(!loader.remember_request(key.clone()));
        loader.cancel_pending();

        assert!(!loader.remember_request(key));
    }

    #[test]
    fn snapshot_event_is_deferred_while_window_is_not_ready() {
        assert!(should_defer_snapshot_event(true, false, 1));
        assert!(!should_defer_snapshot_event(false, false, 1));
        assert!(!should_defer_snapshot_event(true, true, 1));
    }

    #[test]
    fn snapshot_event_is_applied_when_defer_retries_are_exhausted() {
        assert!(!should_defer_snapshot_event(true, false, 0));
    }

    #[test]
    fn worker_snapshot_is_prepared_before_it_reaches_the_ui_loop() {
        let event = prepare_worker_event(WorkerEvent::Snapshot(Snapshot {
            current: None,
            wallpapers: Vec::new(),
            config: AppConfig::default(),
            system_theme: SystemTheme::Dark,
            lock_screen_integration: LockScreenIntegration::default(),
        }));

        let PreparedWorkerEvent::Snapshot(snapshot) = event else {
            panic!("snapshot event should remain a snapshot");
        };

        assert_eq!(snapshot.library_count, 0);
        assert_eq!(snapshot.favorite_count, 0);
        assert_eq!(snapshot.system_theme, SystemTheme::Dark);
    }

    #[test]
    fn thumbnail_decode_groups_share_duplicate_paths() {
        let groups = thumbnail_decode_groups(&[
            thumbnail_request("first", "shared.jpg", 0),
            thumbnail_request("second", "shared.jpg", 1),
            thumbnail_request("third", "unique.jpg", 2),
        ]);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].requests.len(), 2);
        assert_eq!(groups[1].requests.len(), 1);
    }

    fn wallpaper_item(id: &str) -> WallpaperItem {
        wallpaper_item_with_preview(id, "preview.jpg")
    }

    fn wallpaper_item_with_preview(id: &str, preview_path: &str) -> WallpaperItem {
        WallpaperItem {
            id: id.into(),
            title: "Title".into(),
            details: "Details".into(),
            image_path: "image.jpg".into(),
            info_url: "".into(),
            preview_path: preview_path.into(),
            thumbnail: Image::default(),
            thumbnail_ready: false,
            favorite: false,
        }
    }

    fn test_list_thumbnail_loader() -> ListThumbnailLoader {
        ListThumbnailLoader {
            state: Arc::new(ListThumbnailLoaderState {
                pending: Mutex::new(None),
                active_request: Mutex::new(None),
                changed: Condvar::new(),
                generation: AtomicU64::new(0),
            }),
        }
    }

    fn test_current_preview_loader() -> CurrentPreviewLoader {
        CurrentPreviewLoader {
            state: Arc::new(CurrentPreviewLoaderState {
                pending: Mutex::new(None),
                active_request: Mutex::new(None),
                changed: Condvar::new(),
                generation: AtomicU64::new(0),
            }),
        }
    }

    fn list_thumbnail_request_key(id: &str, path: &str) -> super::ListThumbnailRequestKey {
        super::ListThumbnailRequestKey::from_requests(&[ImageLoadRequest {
            id: id.to_string(),
            path: path.into(),
            path_text: path.to_string(),
            row: Some(0),
        }])
    }

    fn current_preview_request_key(id: &str, path: &str) -> super::CurrentPreviewRequestKey {
        super::CurrentPreviewRequestKey::from_request(&ImageLoadRequest {
            id: id.to_string(),
            path: path.into(),
            path_text: path.to_string(),
            row: None,
        })
    }

    fn thumbnail_request(id: &str, path: &str, row: usize) -> ImageLoadRequest {
        ImageLoadRequest {
            id: id.to_string(),
            path: PathBuf::from(path),
            path_text: path.to_string(),
            row: Some(row),
        }
    }

    fn test_image() -> Image {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(1, 1);
        buffer
            .make_mut_bytes()
            .copy_from_slice(&[255, 255, 255, 255]);
        Image::from_rgba8(buffer)
    }
}
