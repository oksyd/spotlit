use std::{
    cmp::Ordering,
    fs,
    num::NonZeroU16,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};

use crate::core::{
    Result, SpotlitError,
    config::{backup_invalid_json, write_json_file},
    fs_utils::remove_file_if_exists,
    model::{LibraryMaintenanceReport, LibraryState, SpotlightMetadata, Wallpaper, WallpaperId},
    retention::history_removal_candidates,
    spotlight::ScannedWallpaper,
};

#[derive(Debug, Clone)]
pub struct WallpaperLibrary {
    path: PathBuf,
    state: LibraryState,
}

impl WallpaperLibrary {
    pub fn load(path: PathBuf) -> Result<Self> {
        if !path.exists() {
            let library = Self {
                path,
                state: LibraryState::default(),
            };
            library.save()?;
            return Ok(library);
        }

        let contents =
            fs::read_to_string(&path).map_err(|source| SpotlitError::io(&path, source))?;
        let mut needs_save = false;
        let state = match serde_json::from_str(&contents) {
            Ok(state) => state,
            Err(source) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %source,
                    "library file is invalid; backing it up and creating an empty library"
                );
                backup_invalid_json(&path)?;
                needs_save = true;
                LibraryState::default()
            }
        };

        let mut library = Self { path, state };
        needs_save |= library.normalize_loaded_wallpapers() > 0;
        let maintenance = library.prune_missing_files();
        needs_save |= maintenance.has_changes();

        if needs_save || library.state.wallpapers.is_empty() {
            library.save()?;
        }
        Ok(library)
    }

    pub fn save(&self) -> Result<()> {
        write_json_file(&self.path, &self.state)
    }

    pub fn upsert_many(
        &mut self,
        scanned: Vec<ScannedWallpaper>,
    ) -> Result<crate::core::ScanReport> {
        let mut inserted = 0;
        let mut updated = 0;

        for item in scanned {
            let now = Utc::now();
            if let Some(existing) = self.state.wallpapers.get_mut(&item.id) {
                let preserve_named_cache =
                    existing.sha256 == item.sha256 && existing.cached_path.exists();
                let scanned_cached_path = item.cached_path;
                let scanned_thumbnail_path = item.thumbnail_path;

                existing.source_path = item.source_path;
                existing.width = item.width;
                existing.height = item.height;
                existing.sha256 = item.sha256;
                existing.last_seen_at = Some(now);

                if preserve_named_cache {
                    remove_distinct_file_if_exists(&scanned_cached_path, &existing.cached_path)?;
                    if existing
                        .thumbnail_path
                        .as_ref()
                        .is_some_and(|path| path.exists())
                    {
                        if let Some(scanned_thumbnail_path) = scanned_thumbnail_path {
                            remove_distinct_file_if_exists(
                                &scanned_thumbnail_path,
                                existing
                                    .thumbnail_path
                                    .as_ref()
                                    .expect("thumbnail path exists"),
                            )?;
                        }
                    } else {
                        existing.thumbnail_path = scanned_thumbnail_path;
                    }
                } else {
                    existing.cached_path = scanned_cached_path;
                    existing.thumbnail_path = scanned_thumbnail_path;
                }

                updated += 1;
            } else {
                self.state.wallpapers.insert(
                    item.id.clone(),
                    Wallpaper {
                        id: item.id,
                        source_path: item.source_path,
                        cached_path: item.cached_path,
                        thumbnail_path: item.thumbnail_path,
                        favorite_path: None,
                        spotlight: SpotlightMetadata::default(),
                        width: item.width,
                        height: item.height,
                        sha256: item.sha256,
                        discovered_at: now,
                        last_seen_at: Some(now),
                        favorited_at: None,
                        last_synced_at: None,
                    },
                );
                inserted += 1;
            }
        }

        Ok(crate::core::ScanReport { inserted, updated })
    }

    pub fn update_spotlight_metadata(
        &mut self,
        id: &WallpaperId,
        metadata: SpotlightMetadata,
    ) -> Result<Wallpaper> {
        let wallpaper = self
            .state
            .wallpapers
            .get_mut(id)
            .ok_or_else(|| SpotlitError::WallpaperNotFound(id.to_string()))?;

        wallpaper.spotlight = metadata.normalized();
        Ok(wallpaper.clone())
    }

    pub fn backfill_spotlight_metadata(
        &mut self,
        id: &WallpaperId,
        metadata: SpotlightMetadata,
    ) -> Result<Option<Wallpaper>> {
        let wallpaper = self
            .state
            .wallpapers
            .get_mut(id)
            .ok_or_else(|| SpotlitError::WallpaperNotFound(id.to_string()))?;

        if wallpaper.spotlight.merge_missing(metadata) {
            return Ok(Some(wallpaper.clone()));
        }

        Ok(None)
    }

    pub fn update_wallpaper_paths(
        &mut self,
        id: &WallpaperId,
        cached_path: PathBuf,
        thumbnail_path: Option<PathBuf>,
        favorite_path: Option<PathBuf>,
    ) -> Result<Wallpaper> {
        let wallpaper = self
            .state
            .wallpapers
            .get_mut(id)
            .ok_or_else(|| SpotlitError::WallpaperNotFound(id.to_string()))?;

        wallpaper.cached_path = cached_path;
        wallpaper.thumbnail_path = thumbnail_path;
        wallpaper.favorite_path = favorite_path;
        Ok(wallpaper.clone())
    }

    pub fn prune_missing_files(&mut self) -> LibraryMaintenanceReport {
        let mut report = LibraryMaintenanceReport::default();

        self.state.wallpapers.retain(|_, wallpaper| {
            let keep = wallpaper.cached_path.exists();
            if !keep {
                report.removed_missing_wallpapers += 1;
            }
            keep
        });

        for wallpaper in self.state.wallpapers.values_mut() {
            if wallpaper
                .thumbnail_path
                .as_ref()
                .is_some_and(|path| !path.exists())
            {
                wallpaper.thumbnail_path = None;
                report.cleared_missing_thumbnails += 1;
            }

            if wallpaper
                .favorite_path
                .as_ref()
                .is_some_and(|path| !path.exists())
            {
                wallpaper.favorite_path = None;
                wallpaper.favorited_at = None;
                report.cleared_missing_favorites += 1;
            }
        }

        report
    }

    fn normalize_loaded_wallpapers(&mut self) -> usize {
        let mut normalized = 0;
        for wallpaper in self.state.wallpapers.values_mut() {
            if wallpaper.last_seen_at.is_none() {
                wallpaper.last_seen_at = Some(wallpaper.discovered_at);
                normalized += 1;
            }
        }
        normalized
    }

    pub fn remove_unretained_cache(&mut self, retain_id: &WallpaperId) -> Result<usize> {
        let removable_ids: Vec<_> = self
            .state
            .wallpapers
            .iter()
            .filter(|(id, wallpaper)| *id != retain_id && !wallpaper.is_favorite())
            .map(|(id, _)| id.clone())
            .collect();

        let mut removed = 0;
        for id in removable_ids {
            let Some(wallpaper) = self.state.wallpapers.remove(&id) else {
                continue;
            };

            remove_file_if_exists(&wallpaper.cached_path)?;
            if let Some(thumbnail_path) = wallpaper.thumbnail_path {
                remove_file_if_exists(&thumbnail_path)?;
            }
            removed += 1;
        }

        Ok(removed)
    }

    pub fn enforce_history_limit(
        &mut self,
        retain_id: Option<&WallpaperId>,
        max_history_wallpapers: Option<NonZeroU16>,
    ) -> Result<usize> {
        let removable_ids = history_removal_candidates(
            self.state.wallpapers.iter(),
            retain_id,
            max_history_wallpapers,
        );

        let mut removed = 0;
        for id in removable_ids {
            let Some(wallpaper) = self.state.wallpapers.remove(&id) else {
                continue;
            };

            remove_file_if_exists(&wallpaper.cached_path)?;
            if let Some(thumbnail_path) = wallpaper.thumbnail_path {
                remove_file_if_exists(&thumbnail_path)?;
            }
            removed += 1;
        }

        Ok(removed)
    }

    pub fn list(&self) -> Vec<Wallpaper> {
        let mut wallpapers: Vec<_> = self.state.wallpapers.values().cloned().collect();
        wallpapers.sort_by_key(|wallpaper| {
            std::cmp::Reverse((wallpaper.seen_at(), wallpaper.discovered_at))
        });
        wallpapers
    }

    pub fn list_favorites(&self) -> Vec<Wallpaper> {
        self.list()
            .into_iter()
            .filter(Wallpaper::is_favorite)
            .collect()
    }

    pub fn rotation_candidate(&self, favorites_only: bool) -> Option<Wallpaper> {
        self.state
            .wallpapers
            .values()
            .filter(|wallpaper| !favorites_only || wallpaper.is_favorite())
            .min_by(|left, right| sync_rotation_order(left, right))
            .cloned()
    }

    pub fn latest_rotation_sync_at(&self, favorites_only: bool) -> Option<DateTime<Utc>> {
        self.state
            .wallpapers
            .values()
            .filter(|wallpaper| !favorites_only || wallpaper.is_favorite())
            .filter_map(|wallpaper| wallpaper.last_synced_at)
            .max()
    }

    pub fn most_recent(&self) -> Option<Wallpaper> {
        self.list().into_iter().next()
    }

    pub fn get(&self, id: &WallpaperId) -> Option<Wallpaper> {
        self.state.wallpapers.get(id).cloned()
    }

    pub fn remove(&mut self, id: &WallpaperId) -> Option<Wallpaper> {
        self.state.wallpapers.remove(id)
    }

    pub fn wallpapers_mut(&mut self) -> impl Iterator<Item = &mut Wallpaper> {
        self.state.wallpapers.values_mut()
    }

    pub fn set_favorite(
        &mut self,
        id: &WallpaperId,
        favorite: bool,
        favorite_path: Option<PathBuf>,
    ) -> Result<crate::core::FavoriteUpdate> {
        let wallpaper = self
            .state
            .wallpapers
            .get_mut(id)
            .ok_or_else(|| SpotlitError::WallpaperNotFound(id.to_string()))?;

        wallpaper.favorited_at = favorite.then(chrono::Utc::now);
        wallpaper.favorite_path = favorite.then_some(favorite_path).flatten();

        Ok(crate::core::FavoriteUpdate {
            id: id.clone(),
            favorite,
        })
    }

    pub fn mark_synced(&mut self, id: &WallpaperId) -> Result<crate::core::SyncReport> {
        let wallpaper = self
            .state
            .wallpapers
            .get_mut(id)
            .ok_or_else(|| SpotlitError::WallpaperNotFound(id.to_string()))?;

        let synced_at = Utc::now();
        wallpaper.last_synced_at = Some(synced_at);

        Ok(crate::core::SyncReport {
            id: id.clone(),
            image_path: wallpaper.cached_path.clone(),
            synced_at,
        })
    }
}

fn remove_distinct_file_if_exists(path: &Path, retained_path: &Path) -> Result<()> {
    if path != retained_path {
        remove_file_if_exists(path)?;
    }
    Ok(())
}

fn sync_rotation_order(left: &Wallpaper, right: &Wallpaper) -> Ordering {
    match (left.last_synced_at, right.last_synced_at) {
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left_synced_at), Some(right_synced_at)) => left_synced_at
            .cmp(&right_synced_at)
            .then_with(|| right.seen_at().cmp(&left.seen_at()))
            .then_with(|| left.id.cmp(&right.id)),
        (None, None) => right
            .seen_at()
            .cmp(&left.seen_at())
            .then_with(|| left.id.cmp(&right.id)),
    }
}
