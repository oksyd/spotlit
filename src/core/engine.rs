use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};

use crate::core::{
    AppConfig, AppPaths, ConfigStore, FavoriteUpdate, LibraryMaintenanceReport, Result, ScanReport,
    SpotlightMetadata, SpotlightScanner, SpotlitError, SyncReport, Wallpaper, WallpaperId,
    WallpaperLibrary, WallpaperSource, ensure_thumbnail, fs_utils::remove_file_if_exists,
    thumbnail::thumbnail_path_for_image,
};

const LOCK_SCREEN_STAGING_RETAINED_FILES: usize = 4;

pub struct SpotlitCore {
    paths: AppPaths,
    sources: Vec<PathBuf>,
    config_store: ConfigStore,
    config: AppConfig,
    library: WallpaperLibrary,
}

impl SpotlitCore {
    pub fn open(paths: AppPaths, sources: Vec<PathBuf>) -> Result<Self> {
        paths.ensure_dirs()?;

        let config_store = ConfigStore::new(paths.config_file.clone());
        let config = config_store.load_or_default()?;
        let library = WallpaperLibrary::load(paths.library_file.clone())?;

        Ok(Self {
            paths,
            sources,
            config_store,
            config,
            library,
        })
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn update_config(&mut self, config: AppConfig) -> Result<()> {
        let config = config.normalized();
        self.config_store.save(&config)?;
        self.config = config;
        Ok(())
    }

    pub fn scan_spotlight_wallpapers(&mut self) -> Result<ScanReport> {
        let scanner = SpotlightScanner::new(self.sources.clone());
        let scanned =
            scanner.scan_into_cache(&self.paths.wallpaper_dir, &self.paths.thumbnail_dir)?;
        let report = self.library.upsert_many(scanned)?;
        self.enforce_history_limit_in_place()?;
        self.library.save()?;
        Ok(report)
    }

    pub fn scan_spotlight_wallpapers_deferred_thumbnails(&mut self) -> Result<ScanReport> {
        let scanner = SpotlightScanner::new(self.sources.clone());
        let scanned = scanner.scan_into_cache_deferred_thumbnails(
            &self.paths.wallpaper_dir,
            &self.paths.thumbnail_dir,
        )?;
        let report = self.library.upsert_many(scanned)?;
        self.enforce_history_limit_in_place()?;
        self.library.save()?;
        Ok(report)
    }

    pub fn maintain_library(&mut self) -> Result<LibraryMaintenanceReport> {
        let mut report = self.library.prune_missing_files();
        report.normalized_wallpapers += self.normalize_metadata_file_names()?;
        report.regenerated_thumbnails = self.regenerate_missing_thumbnails()?;
        report.removed_unretained_wallpapers += self.enforce_history_limit_in_place()?;
        if report.has_changes() {
            self.library.save()?;
        }
        Ok(report)
    }

    pub fn maintain_library_lightweight(&mut self) -> Result<LibraryMaintenanceReport> {
        let mut report = self.library.prune_missing_files();
        report.normalized_wallpapers += self.normalize_metadata_file_names()?;
        report.removed_unretained_wallpapers += self.enforce_history_limit_in_place()?;
        if report.has_changes() {
            self.library.save()?;
        }
        Ok(report)
    }

    pub fn warm_thumbnail_cache(&mut self, limit: usize) -> Result<LibraryMaintenanceReport> {
        let mut report = self.library.prune_missing_files();
        report.normalized_wallpapers += self.normalize_metadata_file_names()?;
        report.regenerated_thumbnails = self.regenerate_missing_thumbnails_limited(limit)?;
        report.removed_unretained_wallpapers += self.enforce_history_limit_in_place()?;
        if report.has_changes() {
            self.library.save()?;
        }
        Ok(report)
    }

    pub fn enforce_history_limit(&mut self) -> Result<LibraryMaintenanceReport> {
        let removed_unretained_wallpapers = self.enforce_history_limit_in_place()?;
        let report = LibraryMaintenanceReport {
            removed_unretained_wallpapers,
            ..LibraryMaintenanceReport::default()
        };
        if report.has_changes() {
            self.library.save()?;
        }
        Ok(report)
    }

    pub fn trim_unretained_cache(&mut self) -> Result<LibraryMaintenanceReport> {
        let current = self
            .current_wallpaper()
            .ok_or(SpotlitError::NoWallpaperAvailable)?;
        let removed_unretained_wallpapers = self.library.remove_unretained_cache(&current.id)?;
        let mut report = self.library.prune_missing_files();
        report.removed_unretained_wallpapers = removed_unretained_wallpapers;

        if report.has_changes() {
            self.library.save()?;
        }

        Ok(report)
    }

    pub fn list_wallpapers(&self) -> Vec<Wallpaper> {
        self.library.list()
    }

    pub fn list_favorites(&self) -> Vec<Wallpaper> {
        self.library.list_favorites()
    }

    pub fn wallpaper_rotation_candidate(&self, source: WallpaperSource) -> Option<Wallpaper> {
        match source {
            WallpaperSource::CurrentDesktop => self.current_wallpaper(),
            WallpaperSource::RandomLibrary => self.library.rotation_candidate(false),
            WallpaperSource::RandomFavorites => self.library.rotation_candidate(true),
        }
    }

    pub fn latest_wallpaper_rotation_sync_at(
        &self,
        source: WallpaperSource,
    ) -> Option<DateTime<Utc>> {
        match source {
            WallpaperSource::CurrentDesktop => self
                .current_wallpaper()
                .and_then(|wallpaper| wallpaper.last_synced_at),
            WallpaperSource::RandomLibrary => self.library.latest_rotation_sync_at(false),
            WallpaperSource::RandomFavorites => self.library.latest_rotation_sync_at(true),
        }
    }

    pub fn current_wallpaper(&self) -> Option<Wallpaper> {
        self.library.most_recent()
    }

    pub fn wallpaper(&self, id: &WallpaperId) -> Option<Wallpaper> {
        self.library.get(id)
    }

    pub fn set_favorite(&mut self, id: &WallpaperId, favorite: bool) -> Result<FavoriteUpdate> {
        let favorite_path = if favorite {
            let wallpaper = self
                .library
                .get(id)
                .ok_or_else(|| SpotlitError::WallpaperNotFound(id.to_string()))?;
            Some(self.copy_to_favorites(&wallpaper)?)
        } else {
            self.remove_from_favorites(id)?;
            None
        };

        let update = self.library.set_favorite(id, favorite, favorite_path)?;
        self.library.save()?;
        Ok(update)
    }

    pub fn remove_wallpaper(&mut self, id: &WallpaperId) -> Result<Wallpaper> {
        let wallpaper = self
            .library
            .remove(id)
            .ok_or_else(|| SpotlitError::WallpaperNotFound(id.to_string()))?;

        self.remove_wallpaper_files(&wallpaper)?;
        self.library.save()?;
        Ok(wallpaper)
    }

    pub fn export_wallpaper(&self, id: &WallpaperId, target_path: &Path) -> Result<PathBuf> {
        let wallpaper = self
            .library
            .get(id)
            .ok_or_else(|| SpotlitError::WallpaperNotFound(id.to_string()))?;

        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).map_err(|source| SpotlitError::io(parent, source))?;
        }

        fs::copy(wallpaper.best_image_path(), target_path)
            .map_err(|source| SpotlitError::io(target_path, source))?;
        Ok(target_path.to_path_buf())
    }

    pub fn import_wallpaper_file(&mut self, path: &Path) -> Result<Option<Wallpaper>> {
        let scanner = SpotlightScanner::new(Vec::new());
        let Some(scanned) = scanner.scan_file_into_cache(
            path,
            &self.paths.wallpaper_dir,
            &self.paths.thumbnail_dir,
        )?
        else {
            return Ok(None);
        };

        let id = scanned.id.clone();
        self.library.upsert_many(vec![scanned])?;
        self.enforce_history_limit_in_place()?;
        self.library.save()?;
        Ok(self.library.get(&id))
    }

    pub fn import_wallpaper_file_deferred_thumbnail(
        &mut self,
        path: &Path,
    ) -> Result<Option<Wallpaper>> {
        let scanner = SpotlightScanner::new(Vec::new());
        let Some(scanned) = scanner.scan_file_into_cache_deferred_thumbnail(
            path,
            &self.paths.wallpaper_dir,
            &self.paths.thumbnail_dir,
        )?
        else {
            return Ok(None);
        };

        let id = scanned.id.clone();
        self.library.upsert_many(vec![scanned])?;
        self.enforce_history_limit_in_place()?;
        self.library.save()?;
        Ok(self.library.get(&id))
    }

    pub fn update_wallpaper_spotlight_metadata(
        &mut self,
        id: &WallpaperId,
        metadata: SpotlightMetadata,
    ) -> Result<Wallpaper> {
        let metadata = metadata.normalized();
        self.library.update_spotlight_metadata(id, metadata)?;
        let wallpaper = self.normalize_wallpaper_file_name(id)?;
        self.library.save()?;
        Ok(wallpaper)
    }

    pub fn backfill_wallpaper_spotlight_metadata(
        &mut self,
        id: &WallpaperId,
        metadata: SpotlightMetadata,
    ) -> Result<Option<Wallpaper>> {
        let Some(_) = self.library.backfill_spotlight_metadata(id, metadata)? else {
            return Ok(None);
        };

        let wallpaper = self.normalize_wallpaper_file_name(id)?;
        self.library.save()?;
        Ok(Some(wallpaper))
    }

    pub fn wallpaper_for_sync(&mut self, preferred_source: Option<&Path>) -> Result<Wallpaper> {
        if let Some(path) = preferred_source
            && let Some(wallpaper) = self.import_wallpaper_file_deferred_thumbnail(path)?
        {
            return Ok(wallpaper);
        }

        self.current_wallpaper()
            .ok_or(SpotlitError::NoWallpaperAvailable)
    }

    pub fn wallpaper_for_sync_by_id(&self, id: &WallpaperId) -> Result<Wallpaper> {
        self.library
            .get(id)
            .ok_or_else(|| SpotlitError::WallpaperNotFound(id.to_string()))
    }

    pub fn prepare_lock_screen_image(&self, wallpaper: &Wallpaper) -> Result<PathBuf> {
        fs::create_dir_all(&self.paths.lock_screen_dir)
            .map_err(|source| SpotlitError::io(&self.paths.lock_screen_dir, source))?;

        // WinRT rejects repeated lock screen updates that reuse the previous file name.
        let staged_path = self.unique_lock_screen_image_path(wallpaper);
        fs::copy(wallpaper.best_image_path(), &staged_path)
            .map_err(|source| SpotlitError::io(&staged_path, source))?;

        let removed = self.prune_lock_screen_staging(LOCK_SCREEN_STAGING_RETAINED_FILES)?;
        if removed > 0 {
            tracing::debug!(removed, "removed stale lock screen image staging files");
        }

        Ok(staged_path)
    }

    pub fn record_lock_screen_sync(&mut self, id: &WallpaperId) -> Result<SyncReport> {
        let report = self.library.mark_synced(id)?;
        let removed = self
            .library
            .enforce_history_limit(Some(id), self.config.max_history_wallpapers)?;
        if removed > 0 {
            tracing::info!(
                removed,
                "removed cached wallpapers outside the history limit after lock screen sync"
            );
        }
        self.library.save()?;
        Ok(report)
    }

    fn copy_to_favorites(&self, wallpaper: &Wallpaper) -> Result<PathBuf> {
        fs::create_dir_all(&self.paths.favorite_dir)
            .map_err(|source| SpotlitError::io(&self.paths.favorite_dir, source))?;

        let extension = wallpaper
            .cached_path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("jpg");
        let favorite_path =
            self.paths
                .favorite_dir
                .join(format!("{}.{}", wallpaper.file_stem(), extension));

        if !favorite_path.exists() {
            fs::copy(wallpaper.best_image_path(), &favorite_path)
                .map_err(|source| SpotlitError::io(&favorite_path, source))?;
        }

        Ok(favorite_path)
    }

    fn remove_from_favorites(&self, id: &WallpaperId) -> Result<()> {
        let Some(wallpaper) = self.library.get(id) else {
            return Err(SpotlitError::WallpaperNotFound(id.to_string()));
        };

        if let Some(favorite_path) = wallpaper.favorite_path {
            return remove_file_if_exists(&favorite_path);
        }

        remove_file_if_exists(&self.favorite_path_for(&wallpaper))?;
        remove_file_if_exists(&self.legacy_favorite_path_for(&wallpaper))
    }

    fn remove_wallpaper_files(&self, wallpaper: &Wallpaper) -> Result<()> {
        remove_file_if_exists(wallpaper.best_image_path())?;

        if let Some(thumbnail_path) = &wallpaper.thumbnail_path {
            remove_file_if_exists(thumbnail_path)?;
        }

        if let Some(favorite_path) = &wallpaper.favorite_path {
            remove_file_if_exists(favorite_path)?;
        } else if wallpaper.is_favorite() {
            remove_file_if_exists(&self.favorite_path_for(wallpaper))?;
            remove_file_if_exists(&self.legacy_favorite_path_for(wallpaper))?;
        }

        Ok(())
    }

    fn normalize_metadata_file_names(&mut self) -> Result<usize> {
        let ids = self
            .library
            .list()
            .into_iter()
            .filter(|wallpaper| !wallpaper.spotlight.is_empty())
            .map(|wallpaper| wallpaper.id)
            .collect::<Vec<_>>();

        let mut changed = 0;
        for id in ids {
            let (_, renamed) = self.rename_wallpaper_files_for_metadata(&id)?;
            changed += usize::from(renamed);
        }

        Ok(changed)
    }

    fn normalize_wallpaper_file_name(&mut self, id: &WallpaperId) -> Result<Wallpaper> {
        self.rename_wallpaper_files_for_metadata(id)
            .map(|(wallpaper, _)| wallpaper)
    }

    fn rename_wallpaper_files_for_metadata(
        &mut self,
        id: &WallpaperId,
    ) -> Result<(Wallpaper, bool)> {
        let wallpaper = self
            .library
            .get(id)
            .ok_or_else(|| SpotlitError::WallpaperNotFound(id.to_string()))?;

        if wallpaper.spotlight.is_empty() {
            return Ok((wallpaper, false));
        }

        let extension = image_extension(&wallpaper.cached_path);
        let cached_path =
            self.paths
                .wallpaper_dir
                .join(format!("{}.{}", wallpaper.file_stem(), extension));
        let cached_path = move_file_preserving_existing(&wallpaper.cached_path, &cached_path)?;
        let thumbnail_path = self.normalize_thumbnail_path(&wallpaper, &cached_path)?;
        let favorite_path = self.normalize_favorite_path(&wallpaper)?;

        let changed = cached_path != wallpaper.cached_path
            || thumbnail_path != wallpaper.thumbnail_path
            || favorite_path != wallpaper.favorite_path;
        let wallpaper =
            self.library
                .update_wallpaper_paths(id, cached_path, thumbnail_path, favorite_path)?;

        Ok((wallpaper, changed))
    }

    fn normalize_thumbnail_path(
        &self,
        wallpaper: &Wallpaper,
        cached_path: &Path,
    ) -> Result<Option<PathBuf>> {
        if !cached_path.exists() {
            return Ok(wallpaper.thumbnail_path.clone());
        }

        let target = thumbnail_path_for_image(cached_path, &self.paths.thumbnail_dir);
        if let Some(thumbnail_path) = &wallpaper.thumbnail_path
            && thumbnail_path.exists()
        {
            return move_file_preserving_existing(thumbnail_path, &target).map(Some);
        }

        Ok(wallpaper.thumbnail_path.clone())
    }

    fn normalize_favorite_path(&self, wallpaper: &Wallpaper) -> Result<Option<PathBuf>> {
        if !wallpaper.is_favorite() {
            return Ok(None);
        }

        let Some(existing_path) = wallpaper
            .favorite_path
            .clone()
            .or_else(|| self.existing_legacy_favorite_path_for(wallpaper))
        else {
            return Ok(wallpaper.favorite_path.clone());
        };

        if !existing_path.exists() {
            return Ok(wallpaper.favorite_path.clone());
        }

        move_file_preserving_existing(&existing_path, &self.favorite_path_for(wallpaper)).map(Some)
    }

    fn favorite_path_for(&self, wallpaper: &Wallpaper) -> PathBuf {
        let extension = image_extension(&wallpaper.cached_path);
        self.paths
            .favorite_dir
            .join(format!("{}.{}", wallpaper.file_stem(), extension))
    }

    fn legacy_favorite_path_for(&self, wallpaper: &Wallpaper) -> PathBuf {
        let extension = image_extension(&wallpaper.cached_path);
        self.paths
            .favorite_dir
            .join(format!("{}.{}", wallpaper.id.as_str(), extension))
    }

    fn existing_legacy_favorite_path_for(&self, wallpaper: &Wallpaper) -> Option<PathBuf> {
        let path = self.legacy_favorite_path_for(wallpaper);
        path.exists().then_some(path)
    }

    fn unique_lock_screen_image_path(&self, wallpaper: &Wallpaper) -> PathBuf {
        let extension = image_extension(wallpaper.best_image_path());
        let timestamp = unix_time_nanos();

        for attempt in 0..100 {
            let file_name = format!(
                "{}-{timestamp}-{attempt}.{extension}",
                wallpaper.file_stem()
            );
            let path = self.paths.lock_screen_dir.join(file_name);
            if !path.exists() {
                return path;
            }
        }

        self.paths.lock_screen_dir.join(format!(
            "{}-{timestamp}-fallback.{extension}",
            wallpaper.file_stem()
        ))
    }

    fn prune_lock_screen_staging(&self, retained_files: usize) -> Result<usize> {
        let mut files = Vec::new();

        let entries = match fs::read_dir(&self.paths.lock_screen_dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(source) => return Err(SpotlitError::io(&self.paths.lock_screen_dir, source)),
        };

        for entry in entries {
            let entry =
                entry.map_err(|source| SpotlitError::io(&self.paths.lock_screen_dir, source))?;
            let path = entry.path();
            let metadata = entry
                .metadata()
                .map_err(|source| SpotlitError::io(&path, source))?;

            if metadata.is_file() {
                files.push((metadata.modified().unwrap_or(UNIX_EPOCH), path));
            }
        }

        if files.len() <= retained_files {
            return Ok(0);
        }

        files.sort_by_key(|(modified_at, _)| *modified_at);
        let remove_count = files.len() - retained_files;
        let mut removed = 0;

        for (_, path) in files.into_iter().take(remove_count) {
            remove_file_if_exists(&path)?;
            removed += 1;
        }

        Ok(removed)
    }

    fn enforce_history_limit_in_place(&mut self) -> Result<usize> {
        let retain_id = self.current_wallpaper().map(|wallpaper| wallpaper.id);
        self.library
            .enforce_history_limit(retain_id.as_ref(), self.config.max_history_wallpapers)
    }

    fn regenerate_missing_thumbnails(&mut self) -> Result<usize> {
        self.regenerate_missing_thumbnails_limited(usize::MAX)
    }

    fn regenerate_missing_thumbnails_limited(&mut self, limit: usize) -> Result<usize> {
        if limit == 0 {
            return Ok(0);
        }

        let thumbnail_dir = self.paths.thumbnail_dir.clone();
        let mut regenerated = 0;

        for wallpaper in self.library.wallpapers_mut() {
            let missing_thumbnail = wallpaper
                .thumbnail_path
                .as_ref()
                .is_none_or(|path| !path.exists());
            if !missing_thumbnail {
                continue;
            }

            let thumbnail_path = ensure_thumbnail(wallpaper.best_image_path(), &thumbnail_dir)?;
            wallpaper.thumbnail_path = Some(thumbnail_path);
            regenerated += 1;

            if regenerated >= limit {
                break;
            }
        }

        Ok(regenerated)
    }
}

fn unix_time_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn image_extension(path: &Path) -> &str {
    path.extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("jpg")
}

fn move_file_preserving_existing(source: &Path, target: &Path) -> Result<PathBuf> {
    if source == target {
        return Ok(target.to_path_buf());
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source| SpotlitError::io(parent, source))?;
    }

    if target.exists() {
        remove_file_if_exists(source)?;
        return Ok(target.to_path_buf());
    }

    if !source.exists() {
        return Ok(source.to_path_buf());
    }

    match fs::rename(source, target) {
        Ok(()) => Ok(target.to_path_buf()),
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            fs::copy(source, target).map_err(|source| SpotlitError::io(target, source))?;
            remove_file_if_exists(source)?;
            Ok(target.to_path_buf())
        }
        Err(source) => Err(SpotlitError::io(target, source)),
    }
}
