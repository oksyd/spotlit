use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::Arc,
};

use crate::core::{AppConfig, LanguageMode, ThemeMode, Wallpaper, WallpaperSource};
use slint::{Image, Model, ModelRc, SharedString, VecModel};

use crate::{
    MainWindow, WallpaperItem, image_cache,
    platform::{
        self, LockScreenBlurMode, LockScreenDisplayMode, LockScreenIntegration,
        LockScreenIntegrationState, SystemTheme,
    },
    worker::{SettingsSnapshot, Snapshot},
};

#[derive(Clone)]
pub(crate) struct PreparedSnapshot {
    pub(crate) current: Option<PreparedWallpaperItem>,
    pub(crate) wallpapers: Arc<[PreparedWallpaperItem]>,
    pub(crate) library_count: usize,
    pub(crate) favorite_count: usize,
    pub(crate) config: AppConfig,
    pub(crate) system_theme: SystemTheme,
    pub(crate) lock_screen_integration: LockScreenIntegration,
    pub(crate) ui_signature: u64,
}

#[derive(Clone, Hash)]
pub(crate) struct PreparedWallpaperItem {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) details: String,
    pub(crate) sync_timestamp: Option<String>,
    pub(crate) image_path: String,
    pub(crate) info_url: String,
    pub(crate) preview_path: String,
    pub(crate) favorite: bool,
}

impl PreparedSnapshot {
    pub(crate) fn settings_only(settings: &SettingsSnapshot) -> Self {
        Self {
            current: None,
            wallpapers: Vec::new().into(),
            library_count: 0,
            favorite_count: 0,
            config: settings.config.clone(),
            system_theme: settings.system_theme,
            lock_screen_integration: settings.lock_screen_integration,
            ui_signature: prepared_snapshot_signature(
                None,
                &[],
                0,
                0,
                &settings.config,
                settings.system_theme,
                settings.lock_screen_integration,
            ),
        }
    }

    pub(crate) fn with_settings(&self, settings: &SettingsSnapshot) -> Self {
        let ui_signature = prepared_snapshot_signature(
            self.current.as_ref(),
            &self.wallpapers,
            self.library_count,
            self.favorite_count,
            &settings.config,
            settings.system_theme,
            settings.lock_screen_integration,
        );

        Self {
            current: self.current.clone(),
            wallpapers: Arc::clone(&self.wallpapers),
            library_count: self.library_count,
            favorite_count: self.favorite_count,
            config: settings.config.clone(),
            system_theme: settings.system_theme,
            lock_screen_integration: settings.lock_screen_integration,
            ui_signature,
        }
    }
}

pub(crate) fn prepare_snapshot(snapshot: Snapshot) -> PreparedSnapshot {
    let current = snapshot.current.as_ref().map(prepare_wallpaper_item);
    let library_count = snapshot.wallpapers.len();
    let favorite_count = snapshot
        .wallpapers
        .iter()
        .filter(|wallpaper| wallpaper.is_favorite())
        .count();
    let wallpapers: Arc<[PreparedWallpaperItem]> = snapshot
        .wallpapers
        .iter()
        .map(prepare_wallpaper_item)
        .collect::<Vec<_>>()
        .into();

    let ui_signature = prepared_snapshot_signature(
        current.as_ref(),
        &wallpapers,
        library_count,
        favorite_count,
        &snapshot.config,
        snapshot.system_theme,
        snapshot.lock_screen_integration,
    );

    PreparedSnapshot {
        current,
        wallpapers,
        library_count,
        favorite_count,
        config: snapshot.config,
        system_theme: snapshot.system_theme,
        lock_screen_integration: snapshot.lock_screen_integration,
        ui_signature,
    }
}

pub(crate) fn apply_prepared_snapshot(app: &MainWindow, snapshot: &PreparedSnapshot) {
    crate::i18n::select_language(snapshot.config.language);

    let previous_current_id = app.get_current_id();
    let previous_current_image_path = app.get_current_image_path();
    let previous_current_preview = app.get_current_preview();
    let previous_current_preview_path = app.get_current_preview_path();
    let previous_current_preview_ready = app.get_current_preview_ready();

    let current = snapshot
        .current
        .as_ref()
        .map(|wallpaper| to_wallpaper_item(app, wallpaper));
    set_current_preview(
        app,
        &previous_current_id,
        &previous_current_image_path,
        &previous_current_preview,
        &previous_current_preview_path,
        previous_current_preview_ready,
        current.as_ref(),
    );
    set_current(app, current.as_ref());

    let mut items: Vec<_> = snapshot
        .wallpapers
        .iter()
        .map(|wallpaper| to_wallpaper_item(app, wallpaper))
        .collect();
    preserve_loaded_thumbnails(app, &mut items);
    let favorite_items: Vec<_> = items
        .iter()
        .filter(|wallpaper| wallpaper.favorite)
        .cloned()
        .collect();

    reconcile_selection(app, &items);

    apply_wallpaper_model(app, items);
    apply_favorite_wallpaper_model(app, favorite_items);

    set_shared_string_if_changed(
        app.get_library_summary(),
        crate::i18n::wallpaper_count(app, snapshot.library_count),
        |value| app.set_library_summary(value),
    );
    set_shared_string_if_changed(
        app.get_favorite_summary(),
        crate::i18n::favorite_count(app, snapshot.favorite_count),
        |value| app.set_favorite_summary(value),
    );
    apply_config(
        app,
        &snapshot.config,
        snapshot.system_theme,
        snapshot.lock_screen_integration,
    );
}

pub fn apply_settings(app: &MainWindow, settings: SettingsSnapshot) {
    crate::i18n::select_language(settings.config.language);
    apply_config(
        app,
        &settings.config,
        settings.system_theme,
        settings.lock_screen_integration,
    );
}

pub(crate) fn apply_favorite_optimistic(app: &MainWindow, id: &str, favorite: bool) {
    if id.is_empty() {
        return;
    }

    update_current_favorite(app, id, favorite);
    update_selected_favorite(app, id, favorite);
    let updated_item = update_model_favorite(&app.get_wallpapers(), id, favorite);
    if let Some(item) = updated_item.as_ref()
        && apply_favorite_model_update(&app.get_favorite_wallpapers(), &app.get_wallpapers(), item)
    {
        update_favorite_summary(app, app.get_favorite_wallpapers().row_count());
        return;
    }

    rebuild_favorites_from_wallpapers(app);
}

pub(crate) fn apply_selected_preview(app: &MainWindow, image: Image, path: SharedString) {
    if should_keep_preview(
        &app.get_selected_preview(),
        &image,
        app.get_selected_preview_path(),
        &path,
        app.get_selected_preview_ready(),
    ) {
        return;
    }

    app.set_selected_preview(image);
    set_shared_string_if_changed(app.get_selected_preview_path(), path, |value| {
        app.set_selected_preview_path(value);
    });
    set_bool_if_changed(app.get_selected_preview_ready(), true, |value| {
        app.set_selected_preview_ready(value);
    });
}

pub(crate) fn clear_selected_preview(app: &MainWindow) {
    if !app.get_selected_preview_ready() && app.get_selected_preview_path().is_empty() {
        return;
    }

    app.set_selected_preview(Image::default());
    set_shared_string_if_changed(
        app.get_selected_preview_path(),
        SharedString::default(),
        |value| app.set_selected_preview_path(value),
    );
    set_bool_if_changed(app.get_selected_preview_ready(), false, |value| {
        app.set_selected_preview_ready(value);
    });
}

pub(crate) fn release_window_images(app: &MainWindow) {
    clear_current_preview(app);
    clear_selected_preview(app);
    clear_selected_wallpaper_thumbnail(app);
    clear_model_thumbnails(&app.get_wallpapers());
    clear_model_thumbnails(&app.get_favorite_wallpapers());
}

fn update_current_favorite(app: &MainWindow, id: &str, favorite: bool) {
    if app.get_current_id().as_str() == id && app.get_current_favorite() != favorite {
        app.set_current_favorite(favorite);
    }
}

fn update_selected_favorite(app: &MainWindow, id: &str, favorite: bool) {
    if !app.get_has_selection() {
        return;
    }

    let mut selected = app.get_selected_wallpaper();
    if selected.id.as_str() == id && selected.favorite != favorite {
        selected.favorite = favorite;
        app.set_selected_wallpaper(selected);
    }
}

fn update_model_favorite(
    model: &ModelRc<WallpaperItem>,
    id: &str,
    favorite: bool,
) -> Option<WallpaperItem> {
    for row in 0..model.row_count() {
        let Some(mut item) = model.row_data(row) else {
            continue;
        };

        if item.id.as_str() == id {
            if item.favorite != favorite {
                item.favorite = favorite;
                model.set_row_data(row, item.clone());
            }
            return Some(item);
        }
    }

    None
}

fn apply_config(
    app: &MainWindow,
    config: &AppConfig,
    system_theme: SystemTheme,
    integration: LockScreenIntegration,
) {
    set_bool_if_changed(
        app.get_auto_sync_enabled(),
        config.auto_sync_lock_screen,
        |value| app.set_auto_sync_enabled(value),
    );
    set_shared_string_if_changed(
        app.get_apply_target_text(),
        crate::i18n::message(app, platform::wallpaper_apply_target_label()),
        |value| app.set_apply_target_text(value),
    );
    set_bool_if_changed(
        app.get_start_at_login_enabled(),
        config.start_at_login,
        |value| app.set_start_at_login_enabled(value),
    );
    set_bool_if_changed(
        app.get_keep_running_in_background_enabled(),
        config.keep_running_in_background,
        |value| app.set_keep_running_in_background_enabled(value),
    );
    set_bool_if_changed(
        app.get_automatic_update_checks_enabled(),
        config.automatic_update_checks,
        |value| app.set_automatic_update_checks_enabled(value),
    );
    set_shared_string_if_changed(
        app.get_language_code(),
        config.language.code().into(),
        |value| app.set_language_code(value),
    );
    set_shared_string_if_changed(
        app.get_history_limit_text(),
        history_limit_label(config).into(),
        |value| app.set_history_limit_text(value),
    );
    set_shared_string_if_changed(
        app.get_wallpaper_source_text(),
        wallpaper_source_label(config.wallpaper_source).into(),
        |value| app.set_wallpaper_source_text(value),
    );
    set_shared_string_if_changed(
        app.get_sync_interval_text(),
        format!("{} minutes", config.sync_interval_minutes).into(),
        |value| app.set_sync_interval_text(value),
    );
    set_shared_string_if_changed(
        app.get_theme_text(),
        theme_label(config.theme, system_theme).into(),
        |value| app.set_theme_text(value),
    );
    set_bool_if_changed(
        app.get_dark_theme(),
        theme_is_dark(config.theme, system_theme),
        |value| app.set_dark_theme(value),
    );
    set_bool_if_changed(
        app.get_gnome_integration_visible(),
        integration.state != LockScreenIntegrationState::Unsupported,
        |value| app.set_gnome_integration_visible(value),
    );
    set_shared_string_if_changed(
        app.get_gnome_integration_state_text(),
        lock_screen_integration_label(integration.state).into(),
        |value| app.set_gnome_integration_state_text(value),
    );
    set_shared_string_if_changed(
        app.get_lock_screen_blur_text(),
        lock_screen_blur_label(integration.blur_mode).into(),
        |value| app.set_lock_screen_blur_text(value),
    );
    set_shared_string_if_changed(
        app.get_lock_screen_display_text(),
        lock_screen_display_label(integration.display_mode).into(),
        |value| app.set_lock_screen_display_text(value),
    );
    set_bool_if_changed(
        app.get_lock_screen_apply_available(),
        matches!(
            integration.state,
            LockScreenIntegrationState::Unsupported | LockScreenIntegrationState::Enabled
        ),
        |value| app.set_lock_screen_apply_available(value),
    );
}

fn rebuild_favorites_from_wallpapers(app: &MainWindow) {
    let wallpapers = app.get_wallpapers();
    let favorites = (0..wallpapers.row_count())
        .filter_map(|row| wallpapers.row_data(row))
        .filter(|item| item.favorite)
        .collect::<Vec<_>>();
    let favorite_count = favorites.len();

    apply_favorite_wallpaper_model(app, favorites);
    update_favorite_summary(app, favorite_count);
}

fn apply_favorite_model_update(
    favorites: &ModelRc<WallpaperItem>,
    wallpapers: &ModelRc<WallpaperItem>,
    item: &WallpaperItem,
) -> bool {
    let Some(favorites_vec) = favorites.as_any().downcast_ref::<VecModel<WallpaperItem>>() else {
        return false;
    };

    let existing_row = model_row_by_id(favorites, item.id.as_str());
    if item.favorite {
        if let Some(row) = existing_row {
            favorites_vec.set_row_data(row, item.clone());
        } else {
            let insert_at = favorite_insert_index(wallpapers, item.id.as_str())
                .unwrap_or_else(|| favorites.row_count())
                .min(favorites.row_count());
            favorites_vec.insert(insert_at, item.clone());
        }
    } else if let Some(row) = existing_row {
        favorites_vec.remove(row);
    }

    true
}

fn model_row_by_id(model: &ModelRc<WallpaperItem>, id: &str) -> Option<usize> {
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

fn favorite_insert_index(model: &ModelRc<WallpaperItem>, id: &str) -> Option<usize> {
    let mut favorite_index = 0usize;

    for row in 0..model.row_count() {
        let Some(item) = model.row_data(row) else {
            continue;
        };

        if item.id.as_str() == id {
            return Some(favorite_index);
        }

        if item.favorite {
            favorite_index += 1;
        }
    }

    None
}

fn update_favorite_summary(app: &MainWindow, favorite_count: usize) {
    set_shared_string_if_changed(
        app.get_favorite_summary(),
        crate::i18n::favorite_count(app, favorite_count),
        |value| app.set_favorite_summary(value),
    );
}

fn apply_wallpaper_model(app: &MainWindow, items: Vec<WallpaperItem>) {
    if !update_model_rows(&app.get_wallpapers(), &items) {
        app.set_wallpapers(ModelRc::new(VecModel::from(items)));
    }
}

fn apply_favorite_wallpaper_model(app: &MainWindow, items: Vec<WallpaperItem>) {
    if !update_model_rows(&app.get_favorite_wallpapers(), &items) {
        app.set_favorite_wallpapers(ModelRc::new(VecModel::from(items)));
    }
}

fn update_model_rows(model: &ModelRc<WallpaperItem>, items: &[WallpaperItem]) -> bool {
    if model.row_count() != items.len() {
        return false;
    }

    for (row, item) in items.iter().enumerate() {
        if model
            .row_data(row)
            .as_ref()
            .is_some_and(|current| same_wallpaper_item(current, item))
        {
            continue;
        }

        model.set_row_data(row, item.clone());
    }

    true
}

fn same_wallpaper_item(current: &WallpaperItem, next: &WallpaperItem) -> bool {
    current.id == next.id
        && current.title == next.title
        && current.details == next.details
        && current.image_path == next.image_path
        && current.info_url == next.info_url
        && current.preview_path == next.preview_path
        && current.thumbnail_ready == next.thumbnail_ready
        && current.favorite == next.favorite
        && (!next.thumbnail_ready
            || image_has_pixels(&current.thumbnail) == image_has_pixels(&next.thumbnail))
}

fn preserve_loaded_thumbnails(app: &MainWindow, items: &mut [WallpaperItem]) {
    let previous = app.get_wallpapers();
    preserve_loaded_thumbnails_from_model(&previous, items);
}

fn preserve_loaded_thumbnails_from_model(
    model: &ModelRc<WallpaperItem>,
    items: &mut [WallpaperItem],
) {
    let preserved_count = image_cache::LIST_THUMBNAIL_CACHE_LIMIT;

    for row in 0..model.row_count().min(preserved_count) {
        let Some(item) = model.row_data(row) else {
            continue;
        };

        if item.thumbnail_ready && image_has_pixels(&item.thumbnail) {
            image_cache::remember_list_thumbnail(
                item.id.as_str(),
                item.preview_path.as_str(),
                &item.thumbnail,
            );
        }
    }

    for item in items.iter_mut().take(preserved_count) {
        let Some(thumbnail) =
            image_cache::list_thumbnail(item.id.as_str(), item.preview_path.as_str())
        else {
            continue;
        };

        item.thumbnail = thumbnail;
        item.thumbnail_ready = true;
    }
}

fn set_current_preview(
    app: &MainWindow,
    previous_current_id: &SharedString,
    previous_current_image_path: &SharedString,
    previous_preview: &Image,
    previous_preview_path: &SharedString,
    previous_preview_ready: bool,
    current: Option<&WallpaperItem>,
) {
    let Some(current) = current else {
        clear_current_preview(app);
        return;
    };

    let preview_source_path = display_preview_source_path(current);
    if previous_current_id == &current.id
        && previous_current_image_path == &current.image_path
        && !preview_source_path.is_empty()
        && previous_preview_path == &preview_source_path
        && previous_preview_ready
        && image_has_pixels(previous_preview)
    {
        image_cache::remember_display_preview(
            current.id.as_str(),
            preview_source_path.as_str(),
            previous_preview,
        );
    } else if let Some(preview) =
        image_cache::display_preview(current.id.as_str(), preview_source_path.as_str())
    {
        apply_current_preview(app, preview, preview_source_path);
    } else if let Some(preview) = current_preview_placeholder(current) {
        apply_current_preview(app, preview, current.preview_path.clone());
    } else {
        clear_current_preview(app);
    }
}

pub(crate) fn apply_current_preview(app: &MainWindow, image: Image, path: SharedString) {
    if should_keep_preview(
        &app.get_current_preview(),
        &image,
        app.get_current_preview_path(),
        &path,
        app.get_current_preview_ready(),
    ) {
        return;
    }

    app.set_current_preview(image);
    set_shared_string_if_changed(app.get_current_preview_path(), path, |value| {
        app.set_current_preview_path(value);
    });
    set_bool_if_changed(app.get_current_preview_ready(), true, |value| {
        app.set_current_preview_ready(value);
    });
}

fn clear_current_preview(app: &MainWindow) {
    if !app.get_current_preview_ready() && app.get_current_preview_path().is_empty() {
        return;
    }

    app.set_current_preview(Image::default());
    set_shared_string_if_changed(
        app.get_current_preview_path(),
        SharedString::default(),
        |value| app.set_current_preview_path(value),
    );
    set_bool_if_changed(app.get_current_preview_ready(), false, |value| {
        app.set_current_preview_ready(value);
    });
}

fn clear_selected_wallpaper_thumbnail(app: &MainWindow) {
    let mut selected = app.get_selected_wallpaper();
    if !selected.thumbnail_ready && !image_has_pixels(&selected.thumbnail) {
        return;
    }

    selected.thumbnail = Image::default();
    selected.thumbnail_ready = false;
    app.set_selected_wallpaper(selected);
}

fn clear_model_thumbnails(model: &ModelRc<WallpaperItem>) {
    for row in 0..model.row_count() {
        let Some(mut item) = model.row_data(row) else {
            continue;
        };

        if !item.thumbnail_ready && !image_has_pixels(&item.thumbnail) {
            continue;
        }

        item.thumbnail = Image::default();
        item.thumbnail_ready = false;
        model.set_row_data(row, item);
    }
}

fn set_current(app: &MainWindow, current: Option<&WallpaperItem>) {
    let Some(current) = current else {
        set_bool_if_changed(app.get_has_current(), false, |value| {
            app.set_has_current(value);
        });
        set_shared_string_if_changed(app.get_current_id(), SharedString::default(), |value| {
            app.set_current_id(value)
        });
        set_shared_string_if_changed(app.get_current_title(), SharedString::default(), |value| {
            app.set_current_title(value)
        });
        set_shared_string_if_changed(
            app.get_current_details(),
            SharedString::default(),
            |value| app.set_current_details(value),
        );
        set_shared_string_if_changed(
            app.get_current_image_path(),
            SharedString::default(),
            |value| app.set_current_image_path(value),
        );
        set_shared_string_if_changed(
            app.get_current_info_url(),
            SharedString::default(),
            |value| app.set_current_info_url(value),
        );
        set_shared_string_if_changed(
            app.get_current_preview_source_path(),
            SharedString::default(),
            |value| app.set_current_preview_source_path(value),
        );
        set_bool_if_changed(app.get_current_favorite(), false, |value| {
            app.set_current_favorite(value);
        });
        return;
    };

    set_bool_if_changed(app.get_has_current(), true, |value| {
        app.set_has_current(value);
    });
    set_shared_string_if_changed(app.get_current_id(), current.id.clone(), |value| {
        app.set_current_id(value);
    });
    set_shared_string_if_changed(app.get_current_title(), current.title.clone(), |value| {
        app.set_current_title(value);
    });
    set_shared_string_if_changed(
        app.get_current_details(),
        current.details.clone(),
        |value| {
            app.set_current_details(value);
        },
    );
    set_shared_string_if_changed(
        app.get_current_image_path(),
        current.image_path.clone(),
        |value| app.set_current_image_path(value),
    );
    set_shared_string_if_changed(
        app.get_current_info_url(),
        current.info_url.clone(),
        |value| {
            app.set_current_info_url(value);
        },
    );
    set_shared_string_if_changed(
        app.get_current_preview_source_path(),
        display_preview_source_path(current),
        |value| app.set_current_preview_source_path(value),
    );
    set_bool_if_changed(app.get_current_favorite(), current.favorite, |value| {
        app.set_current_favorite(value)
    });
}

fn reconcile_selection(app: &MainWindow, items: &[WallpaperItem]) {
    if !app.get_has_selection() {
        return;
    }

    let selected_id = app.get_selected_wallpaper().id.to_string();
    if let Some(selected) = items.iter().find(|item| item.id == selected_id) {
        let previous_selected = app.get_selected_wallpaper();
        if previous_selected.image_path != selected.image_path
            || previous_selected.preview_path != selected.preview_path
        {
            clear_selected_preview(app);
        }

        if !same_wallpaper_item(&previous_selected, selected) {
            app.set_selected_wallpaper(selected.clone());
        }
    } else {
        set_bool_if_changed(app.get_has_selection(), false, |value| {
            app.set_has_selection(value);
        });
        set_bool_if_changed(app.get_show_current_details(), false, |value| {
            app.set_show_current_details(value);
        });
        clear_selected_preview(app);
    }
}

fn prepare_wallpaper_item(wallpaper: &Wallpaper) -> PreparedWallpaperItem {
    let preview_path_text = wallpaper
        .thumbnail_path
        .as_deref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = wallpaper.id.to_string();

    let (details, sync_timestamp) = wallpaper_details(wallpaper);
    PreparedWallpaperItem {
        id,
        title: ui_wallpaper_title(wallpaper),
        details,
        sync_timestamp,
        image_path: wallpaper.best_image_path().to_string_lossy().into_owned(),
        info_url: english_info_url(wallpaper.spotlight.info_url.as_deref())
            .unwrap_or_default()
            .to_string(),
        preview_path: preview_path_text,
        favorite: wallpaper.is_favorite(),
    }
}

fn to_wallpaper_item(app: &MainWindow, wallpaper: &PreparedWallpaperItem) -> WallpaperItem {
    let details = match wallpaper.sync_timestamp.as_deref() {
        Some(timestamp) if wallpaper.details.is_empty() => {
            crate::i18n::sync_time(app, timestamp).to_string()
        }
        Some(timestamp) => format!(
            "{} | {}",
            wallpaper.details,
            crate::i18n::sync_time(app, timestamp)
        ),
        None => wallpaper.details.clone(),
    };

    WallpaperItem {
        id: wallpaper.id.as_str().into(),
        title: wallpaper.title.as_str().into(),
        details: details.into(),
        image_path: wallpaper.image_path.as_str().into(),
        info_url: wallpaper.info_url.as_str().into(),
        preview_path: wallpaper.preview_path.as_str().into(),
        thumbnail: Image::default(),
        thumbnail_ready: false,
        favorite: wallpaper.favorite,
    }
}

fn display_preview_source_path(item: &WallpaperItem) -> SharedString {
    if item.preview_path.is_empty() {
        return item.image_path.clone();
    }

    item.preview_path.clone()
}

fn current_preview_placeholder(item: &WallpaperItem) -> Option<Image> {
    if item.preview_path.is_empty() {
        return None;
    }

    image_cache::display_preview(item.id.as_str(), item.preview_path.as_str())
        .or_else(|| image_cache::list_thumbnail(item.id.as_str(), item.preview_path.as_str()))
        .or_else(|| image_cache::list_thumbnail_by_path(item.preview_path.as_str()))
}

fn image_has_pixels(image: &Image) -> bool {
    let size = image.size();
    size.width > 0 && size.height > 0
}

fn should_keep_preview(
    current: &Image,
    next: &Image,
    current_path: SharedString,
    next_path: &SharedString,
    current_ready: bool,
) -> bool {
    if !current_ready || current_path != *next_path || !image_has_pixels(current) {
        return false;
    }

    let current_size = current.size();
    let next_size = next.size();
    current_size.width >= next_size.width && current_size.height >= next_size.height
}

fn wallpaper_details(wallpaper: &Wallpaper) -> (String, Option<String>) {
    let mut parts = Vec::new();
    let title = ui_wallpaper_title(wallpaper);

    if let Some(caption) = english_visible_text(wallpaper.spotlight.caption.as_deref())
        && !caption.eq_ignore_ascii_case(&title)
    {
        parts.push(caption.to_string());
    }

    if let Some(copyright) = english_visible_text(wallpaper.spotlight.copyright.as_deref()) {
        parts.push(copyright.to_string());
    }

    parts.push(format!("{} x {}", wallpaper.width, wallpaper.height));
    parts.push(wallpaper.seen_at().format("%Y-%m-%d %H:%M").to_string());

    let sync_timestamp = wallpaper
        .last_synced_at
        .map(|timestamp| timestamp.format("%Y-%m-%d %H:%M").to_string());

    (parts.join(" | "), sync_timestamp)
}

fn ui_wallpaper_title(wallpaper: &Wallpaper) -> String {
    english_visible_text(wallpaper.spotlight.title.as_deref())
        .or_else(|| english_visible_text(wallpaper.spotlight.caption.as_deref()))
        .map(ToOwned::to_owned)
        .or_else(|| {
            wallpaper
                .spotlight
                .content_id
                .as_deref()
                .and_then(ohr_slug_title)
        })
        .unwrap_or_else(|| wallpaper.id.to_string())
}

fn english_visible_text(value: Option<&str>) -> Option<&str> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| !contains_cjk_text(value))
}

fn english_info_url(value: Option<&str>) -> Option<&str> {
    english_visible_text(value).filter(|value| !has_chinese_locale_marker(value))
}

fn has_chinese_locale_marker(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.contains("zh-cn")
        || value.contains("zh-hans")
        || value.contains("zh-hant")
        || value.contains("zh-tw")
        || value.contains("zh-hk")
}

fn contains_cjk_text(value: &str) -> bool {
    value.chars().any(|character| {
        matches!(
            character,
            '\u{3400}'..='\u{4DBF}'
                | '\u{4E00}'..='\u{9FFF}'
                | '\u{F900}'..='\u{FAFF}'
        )
    })
}

fn ohr_slug_title(value: &str) -> Option<String> {
    let marker = "OHR.";
    let start = value.find(marker)? + marker.len();
    let slug: String = value[start..]
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .collect();
    if slug.is_empty() {
        return None;
    }

    Some(camel_slug_to_title(&slug))
}

fn camel_slug_to_title(slug: &str) -> String {
    let mut title = String::with_capacity(slug.len() + 8);
    let mut previous: Option<char> = None;
    let mut next = slug.chars().peekable();

    while let Some(character) = next.next() {
        if let Some(previous) = previous {
            let insert_space = character.is_ascii_uppercase()
                && (previous.is_ascii_lowercase()
                    || next.peek().is_some_and(|next| {
                        next.is_ascii_lowercase() && previous.is_ascii_uppercase()
                    }));
            if insert_space {
                title.push(' ');
            }
        }
        title.push(character);
        previous = Some(character);
    }

    title
}

fn set_bool_if_changed(current: bool, next: bool, setter: impl FnOnce(bool)) {
    if current != next {
        setter(next);
    }
}

fn set_shared_string_if_changed(
    current: SharedString,
    next: SharedString,
    setter: impl FnOnce(SharedString),
) {
    if current != next {
        setter(next);
    }
}

fn theme_label(theme: ThemeMode, system_theme: SystemTheme) -> &'static str {
    match (theme, system_theme) {
        (ThemeMode::Light, _) => "Light",
        (ThemeMode::Dark, _) => "Dark",
        (ThemeMode::System, SystemTheme::Light) => "System (Light)",
        (ThemeMode::System, SystemTheme::Dark) => "System (Dark)",
    }
}

fn lock_screen_integration_label(state: LockScreenIntegrationState) -> &'static str {
    match state {
        LockScreenIntegrationState::Unsupported => "Unsupported",
        LockScreenIntegrationState::Unavailable => "Unavailable",
        LockScreenIntegrationState::NotInstalled => "Not Installed",
        LockScreenIntegrationState::RestartRequired => "Restart Required",
        LockScreenIntegrationState::Disabled => "Disabled",
        LockScreenIntegrationState::Enabled => "Ready",
    }
}

fn lock_screen_blur_label(mode: LockScreenBlurMode) -> &'static str {
    match mode {
        LockScreenBlurMode::System => "System",
        LockScreenBlurMode::Soft => "Soft",
        LockScreenBlurMode::Clear => "Clear",
    }
}

fn lock_screen_display_label(mode: LockScreenDisplayMode) -> &'static str {
    match mode {
        LockScreenDisplayMode::System => "System",
        LockScreenDisplayMode::PluggedIn => "Plugged In",
        LockScreenDisplayMode::Always => "Always",
    }
}

fn history_limit_label(config: &AppConfig) -> String {
    match config.max_history_wallpapers {
        Some(limit) => format!("{} wallpapers", limit.get()),
        None => "Unlimited".to_string(),
    }
}

fn wallpaper_source_label(source: WallpaperSource) -> &'static str {
    match source {
        WallpaperSource::CurrentDesktop => "Current Desktop",
        WallpaperSource::RandomLibrary => "Library",
        WallpaperSource::RandomFavorites => "Favorites",
    }
}

fn theme_is_dark(theme: ThemeMode, system_theme: SystemTheme) -> bool {
    match theme {
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
        ThemeMode::System => matches!(system_theme, SystemTheme::Dark),
    }
}

fn prepared_snapshot_signature(
    current: Option<&PreparedWallpaperItem>,
    wallpapers: &[PreparedWallpaperItem],
    library_count: usize,
    favorite_count: usize,
    config: &AppConfig,
    system_theme: SystemTheme,
    lock_screen_integration: LockScreenIntegration,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    library_count.hash(&mut hasher);
    favorite_count.hash(&mut hasher);
    hash_config(config, &mut hasher);
    hash_system_theme(system_theme, &mut hasher);
    lock_screen_integration.hash(&mut hasher);
    current.hash(&mut hasher);
    wallpapers.hash(&mut hasher);
    hasher.finish()
}

fn hash_config(config: &AppConfig, hasher: &mut DefaultHasher) {
    config.auto_sync_lock_screen.hash(hasher);
    hash_wallpaper_source(config.wallpaper_source, hasher);
    config.sync_interval_minutes.hash(hasher);
    hash_theme_mode(config.theme, hasher);
    config.start_at_login.hash(hasher);
    config.keep_running_in_background.hash(hasher);
    config.automatic_update_checks.hash(hasher);
    hash_language_mode(config.language, hasher);
    config.max_history_wallpapers.hash(hasher);
}

fn hash_language_mode(language: LanguageMode, hasher: &mut DefaultHasher) {
    match language {
        LanguageMode::System => 0_u8,
        LanguageMode::English => 1,
        LanguageMode::SimplifiedChinese => 2,
        LanguageMode::German => 3,
    }
    .hash(hasher);
}

fn hash_wallpaper_source(source: WallpaperSource, hasher: &mut DefaultHasher) {
    match source {
        WallpaperSource::CurrentDesktop => 0_u8,
        WallpaperSource::RandomLibrary => 1,
        WallpaperSource::RandomFavorites => 2,
    }
    .hash(hasher);
}

fn hash_theme_mode(theme: ThemeMode, hasher: &mut DefaultHasher) {
    match theme {
        ThemeMode::Light => 0_u8,
        ThemeMode::Dark => 1,
        ThemeMode::System => 2,
    }
    .hash(hasher);
}

fn hash_system_theme(theme: SystemTheme, hasher: &mut DefaultHasher) {
    match theme {
        SystemTheme::Light => 0_u8,
        SystemTheme::Dark => 1,
    }
    .hash(hasher);
}

#[cfg(test)]
mod tests {
    use crate::core::{SpotlightMetadata, Wallpaper, WallpaperId};
    use crate::platform::{LockScreenDisplayMode, LockScreenIntegrationState};
    use chrono::Utc;
    use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

    use super::{
        WallpaperItem, apply_favorite_model_update, display_preview_source_path,
        lock_screen_display_label, lock_screen_integration_label, prepare_wallpaper_item,
        preserve_loaded_thumbnails_from_model, same_wallpaper_item, should_keep_preview,
    };

    #[test]
    fn extension_label_reflects_runtime_state() {
        assert_eq!(
            lock_screen_integration_label(LockScreenIntegrationState::RestartRequired),
            "Restart Required"
        );
        assert_eq!(
            lock_screen_integration_label(LockScreenIntegrationState::Enabled),
            "Ready"
        );
    }

    #[test]
    fn lock_screen_display_label_reflects_power_policy() {
        assert_eq!(
            lock_screen_display_label(LockScreenDisplayMode::PluggedIn),
            "Plugged In"
        );
        assert_eq!(
            lock_screen_display_label(LockScreenDisplayMode::Always),
            "Always"
        );
    }

    #[test]
    fn identical_wallpaper_items_do_not_need_row_update() {
        let current = wallpaper_item("id");
        let next = wallpaper_item("id");

        assert!(same_wallpaper_item(&current, &next));
    }

    #[test]
    fn favorite_change_needs_row_update() {
        let current = wallpaper_item("id");
        let mut next = wallpaper_item("id");
        next.favorite = true;

        assert!(!same_wallpaper_item(&current, &next));
    }

    #[test]
    fn thumbnail_becoming_available_needs_row_update() {
        let mut current = wallpaper_item("id");
        current.thumbnail_ready = true;
        let mut next = wallpaper_item("id");
        next.thumbnail_ready = true;
        next.thumbnail = test_image();

        assert!(!same_wallpaper_item(&current, &next));
    }

    #[test]
    fn prepared_wallpaper_item_filters_chinese_metadata_for_ui() {
        let wallpaper = Wallpaper {
            id: WallpaperId::new("fallback-id"),
            source_path: "source.jpg".into(),
            cached_path: "cached.jpg".into(),
            thumbnail_path: None,
            favorite_path: None,
            spotlight: SpotlightMetadata {
                spotlight_id: Some("430c1f4847a17f50aecd5df4f069b9b9".to_string()),
                title: Some("逐渐失去立足之地的树木".to_string()),
                caption: Some("逐渐失去立足之地的树木".to_string()),
                copyright: Some("博尼亚德海滩上的漂流木, 亨廷岛, 南卡罗来纳州, 美国".to_string()),
                info_url: Some(
                    "https://www.bing.com/search?q=亨廷岛&form=hpcapt&mkt=zh-cn".to_string(),
                ),
                content_id: Some("/th?id=OHR.BoneyardBeach_ZH-CN5540590570".to_string()),
            },
            width: 1920,
            height: 1080,
            sha256: "sha256".to_string(),
            discovered_at: Utc::now(),
            last_seen_at: None,
            favorited_at: None,
            last_synced_at: None,
        };

        let item = prepare_wallpaper_item(&wallpaper);

        assert_eq!(item.title, "Boneyard Beach");
        assert_eq!(item.info_url, "");
        assert!(!item.details.contains("逐渐"));
        assert!(!item.details.contains("博尼亚德"));
        assert!(item.details.contains("1920 x 1080"));
    }

    #[test]
    fn preview_placeholder_is_replaced_by_larger_image() {
        let current = test_image_size(176, 104);
        let next = test_image_size(960, 540);

        assert!(!should_keep_preview(
            &current,
            &next,
            "preview.jpg".into(),
            &"preview.jpg".into(),
            true,
        ));
    }

    #[test]
    fn same_size_preview_is_kept_for_same_path() {
        let current = test_image_size(960, 540);
        let next = test_image_size(960, 540);

        assert!(should_keep_preview(
            &current,
            &next,
            "preview.jpg".into(),
            &"preview.jpg".into(),
            true,
        ));
    }

    #[test]
    fn display_preview_source_prefers_preview_path() {
        let item = wallpaper_item_with_paths("image.jpg", "thumbnail.jpg");

        assert_eq!(display_preview_source_path(&item), "thumbnail.jpg");
    }

    #[test]
    fn display_preview_source_falls_back_to_image_path() {
        let item = wallpaper_item_with_paths("image.jpg", "");

        assert_eq!(display_preview_source_path(&item), "image.jpg");
    }

    #[test]
    fn preserve_loaded_thumbnails_is_limited_to_cache_window() {
        let preserved_count = crate::image_cache::LIST_THUMBNAIL_CACHE_LIMIT;
        let previous_items = (0..preserved_count + 2)
            .map(|index| {
                let mut item = wallpaper_item_with_paths(
                    &format!("preserved-{index}.jpg"),
                    &format!("preserved-{index}-preview.jpg"),
                )
                .with_id(&format!("preserved-{index}"));
                item.thumbnail = test_image();
                item.thumbnail_ready = true;
                item
            })
            .collect::<Vec<_>>();
        let previous_model = ModelRc::new(VecModel::from(previous_items.clone()));
        let mut next_items = previous_items
            .iter()
            .map(|item| {
                wallpaper_item_with_paths(item.image_path.as_str(), item.preview_path.as_str())
                    .with_id(item.id.as_str())
            })
            .collect::<Vec<_>>();

        preserve_loaded_thumbnails_from_model(&previous_model, &mut next_items);

        assert!(
            next_items
                .iter()
                .take(preserved_count)
                .all(|item| item.thumbnail_ready)
        );
        assert!(!next_items[preserved_count].thumbnail_ready);
    }

    #[test]
    fn favorite_model_update_inserts_without_rebuilding_all_rows() {
        let mut first = wallpaper_item("first");
        first.favorite = true;
        let mut middle = wallpaper_item("middle");
        middle.favorite = true;
        let mut last = wallpaper_item("last");
        last.favorite = true;
        let wallpapers = ModelRc::new(VecModel::from(vec![
            first.clone(),
            middle.clone(),
            last.clone(),
        ]));
        let favorites = ModelRc::new(VecModel::from(vec![first, last]));

        assert!(apply_favorite_model_update(
            &favorites,
            &wallpapers,
            &middle
        ));

        assert_eq!(
            model_ids(&favorites),
            vec![
                "first".to_string(),
                "middle".to_string(),
                "last".to_string()
            ]
        );
    }

    #[test]
    fn favorite_model_update_removes_single_unfavorited_row() {
        let mut first = wallpaper_item("first");
        first.favorite = true;
        let mut middle = wallpaper_item("middle");
        middle.favorite = false;
        let mut last = wallpaper_item("last");
        last.favorite = true;
        let wallpapers = ModelRc::new(VecModel::from(vec![
            first.clone(),
            middle.clone(),
            last.clone(),
        ]));
        let favorites = ModelRc::new(VecModel::from(vec![first, middle.clone(), last]));

        assert!(apply_favorite_model_update(
            &favorites,
            &wallpapers,
            &middle
        ));

        assert_eq!(
            model_ids(&favorites),
            vec!["first".to_string(), "last".to_string()]
        );
    }

    fn wallpaper_item(id: &str) -> WallpaperItem {
        wallpaper_item_with_paths("image.jpg", "preview.jpg").with_id(id)
    }

    fn wallpaper_item_with_paths(image_path: &str, preview_path: &str) -> WallpaperItem {
        WallpaperItem {
            id: "id".into(),
            title: "Title".into(),
            details: "Details".into(),
            image_path: image_path.into(),
            info_url: "".into(),
            preview_path: preview_path.into(),
            thumbnail: Image::default(),
            thumbnail_ready: false,
            favorite: false,
        }
    }

    trait WallpaperItemTestExt {
        fn with_id(self, id: &str) -> WallpaperItem;
    }

    impl WallpaperItemTestExt for WallpaperItem {
        fn with_id(mut self, id: &str) -> WallpaperItem {
            self.id = id.into();
            self
        }
    }

    fn model_ids(model: &ModelRc<WallpaperItem>) -> Vec<String> {
        (0..model.row_count())
            .filter_map(|row| model.row_data(row))
            .map(|item| item.id.to_string())
            .collect()
    }

    fn test_image() -> Image {
        test_image_size(1, 1)
    }

    fn test_image_size(width: u32, height: u32) -> Image {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
        buffer.make_mut_bytes().fill(255);
        Image::from_rgba8(buffer)
    }
}
