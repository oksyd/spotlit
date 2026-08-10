use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::{Result, SpotlitError};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub wallpaper_dir: PathBuf,
    pub thumbnail_dir: PathBuf,
    pub favorite_dir: PathBuf,
    pub lock_screen_dir: PathBuf,
    pub log_dir: PathBuf,
    pub config_file: PathBuf,
    pub library_file: PathBuf,
}

impl AppPaths {
    pub fn new(data_dir: PathBuf) -> Self {
        let wallpaper_dir = data_dir.join("wallpapers");
        let thumbnail_dir = data_dir.join("thumbnails");
        let favorite_dir = data_dir.join("favorites");
        let lock_screen_dir = data_dir.join("lock-screen");
        let log_dir = data_dir.join("logs");

        Self {
            config_file: data_dir.join("config.json"),
            library_file: data_dir.join("library.json"),
            data_dir,
            wallpaper_dir,
            thumbnail_dir,
            favorite_dir,
            lock_screen_dir,
            log_dir,
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        for path in [
            &self.data_dir,
            &self.wallpaper_dir,
            &self.thumbnail_dir,
            &self.favorite_dir,
            &self.lock_screen_dir,
            &self.log_dir,
        ] {
            fs::create_dir_all(path).map_err(|source| SpotlitError::io(path, source))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct WallpaperId(String);

impl WallpaperId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WallpaperId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wallpaper {
    pub id: WallpaperId,
    pub source_path: PathBuf,
    pub cached_path: PathBuf,
    pub thumbnail_path: Option<PathBuf>,
    #[serde(default)]
    pub favorite_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "SpotlightMetadata::is_empty")]
    pub spotlight: SpotlightMetadata,
    pub width: u32,
    pub height: u32,
    pub sha256: String,
    pub discovered_at: DateTime<Utc>,
    #[serde(default)]
    pub last_seen_at: Option<DateTime<Utc>>,
    pub favorited_at: Option<DateTime<Utc>>,
    pub last_synced_at: Option<DateTime<Utc>>,
}

impl Wallpaper {
    pub fn best_image_path(&self) -> &Path {
        self.cached_path.as_path()
    }

    pub fn display_title(&self) -> &str {
        self.spotlight
            .display_title()
            .unwrap_or_else(|| self.id.as_str())
    }

    pub fn is_favorite(&self) -> bool {
        self.favorited_at.is_some()
    }

    pub fn seen_at(&self) -> DateTime<Utc> {
        self.last_seen_at.unwrap_or(self.discovered_at)
    }

    pub fn file_stem(&self) -> String {
        readable_wallpaper_file_stem(&self.id, &self.spotlight)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct SpotlightMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spotlight_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
}

impl SpotlightMetadata {
    pub fn normalized(self) -> Self {
        Self {
            spotlight_id: normalize_text(self.spotlight_id),
            title: normalize_text(self.title),
            caption: normalize_text(self.caption),
            copyright: normalize_text(self.copyright),
            info_url: normalize_text(self.info_url),
            content_id: normalize_text(self.content_id),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.spotlight_id.is_none()
            && self.title.is_none()
            && self.caption.is_none()
            && self.copyright.is_none()
            && self.info_url.is_none()
            && self.content_id.is_none()
    }

    pub fn merge_missing(&mut self, metadata: SpotlightMetadata) -> bool {
        let metadata = metadata.normalized();
        let mut changed = false;

        changed |= fill_missing_text(&mut self.spotlight_id, metadata.spotlight_id);
        changed |= fill_missing_text(&mut self.title, metadata.title);
        changed |= fill_missing_text(&mut self.caption, metadata.caption);
        changed |= fill_missing_text(&mut self.copyright, metadata.copyright);
        changed |= fill_missing_text(&mut self.info_url, metadata.info_url);
        changed |= fill_missing_text(&mut self.content_id, metadata.content_id);

        changed
    }

    pub fn display_title(&self) -> Option<&str> {
        self.title
            .as_deref()
            .or(self.caption.as_deref())
            .or(self.spotlight_id.as_deref())
    }

    pub fn file_name_hint(&self) -> Option<&str> {
        self.title
            .as_deref()
            .or(self.caption.as_deref())
            .or(self.spotlight_id.as_deref())
            .or(self.content_id.as_deref())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DesktopSpotlightCreative {
    pub landscape_path: PathBuf,
    pub portrait_path: Option<PathBuf>,
    pub metadata: SpotlightMetadata,
    pub is_current: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibraryState {
    pub wallpapers: BTreeMap<WallpaperId, Wallpaper>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct LibraryMaintenanceReport {
    pub removed_missing_wallpapers: usize,
    pub cleared_missing_thumbnails: usize,
    pub cleared_missing_favorites: usize,
    pub removed_unretained_wallpapers: usize,
    pub normalized_wallpapers: usize,
    pub regenerated_thumbnails: usize,
}

impl LibraryMaintenanceReport {
    pub fn has_changes(self) -> bool {
        self.removed_missing_wallpapers > 0
            || self.cleared_missing_thumbnails > 0
            || self.cleared_missing_favorites > 0
            || self.removed_unretained_wallpapers > 0
            || self.normalized_wallpapers > 0
            || self.regenerated_thumbnails > 0
    }
}

fn normalize_text(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn fill_missing_text(target: &mut Option<String>, value: Option<String>) -> bool {
    if target.is_none() && value.is_some() {
        *target = value;
        return true;
    }

    false
}

fn readable_wallpaper_file_stem(id: &WallpaperId, metadata: &SpotlightMetadata) -> String {
    let Some(slug) = metadata
        .file_name_hint()
        .map(file_stem_slug)
        .filter(|value| !value.is_empty())
    else {
        return id.as_str().to_string();
    };

    format!("{slug}-{id}")
}

fn file_stem_slug(value: &str) -> String {
    const MAX_LEN: usize = 80;

    let mut slug = String::new();
    let mut last_was_separator = false;
    let mut len = 0;

    for character in value.chars() {
        if len >= MAX_LEN {
            break;
        }

        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_separator = false;
            len += 1;
        } else if character.is_alphanumeric() {
            slug.push(character);
            last_was_separator = false;
            len += 1;
        } else if (character.is_whitespace() || matches!(character, '-' | '_' | '.' | ','))
            && !slug.is_empty()
            && !last_was_separator
        {
            slug.push('-');
            last_was_separator = true;
            len += 1;
        }
    }

    slug.trim_matches('-').to_string()
}
