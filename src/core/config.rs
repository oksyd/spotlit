use std::{
    fs,
    io::Write,
    num::NonZeroU16,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::core::{Result, SpotlitError};

pub const DEFAULT_MAX_HISTORY_WALLPAPERS: u16 = 250;
pub const MAX_HISTORY_WALLPAPERS: u16 = 1_000;

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(default)]
pub struct AppConfig {
    pub auto_sync_lock_screen: bool,
    #[serde(alias = "lock_screen_source")]
    pub wallpaper_source: WallpaperSource,
    pub sync_interval_minutes: u32,
    pub theme: ThemeMode,
    #[serde(alias = "start_with_windows")]
    pub start_at_login: bool,
    pub keep_running_in_background: bool,
    pub automatic_update_checks: bool,
    pub language: LanguageMode,
    #[serde(default = "default_max_history_wallpapers")]
    pub max_history_wallpapers: Option<NonZeroU16>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            auto_sync_lock_screen: false,
            wallpaper_source: WallpaperSource::CurrentDesktop,
            sync_interval_minutes: 30,
            theme: ThemeMode::System,
            start_at_login: false,
            keep_running_in_background: true,
            automatic_update_checks: true,
            language: LanguageMode::System,
            max_history_wallpapers: default_max_history_wallpapers(),
        }
    }
}

impl AppConfig {
    pub fn normalized(mut self) -> Self {
        self.sync_interval_minutes = match self.sync_interval_minutes {
            0 => Self::default().sync_interval_minutes,
            minutes => minutes.min(1440),
        };
        self.max_history_wallpapers = self
            .max_history_wallpapers
            .map(|limit| normalized_history_limit(limit.get()));
        self
    }
}

pub fn default_max_history_wallpapers() -> Option<NonZeroU16> {
    NonZeroU16::new(DEFAULT_MAX_HISTORY_WALLPAPERS)
}

pub fn normalized_history_limit(limit: u16) -> NonZeroU16 {
    let limit = limit.clamp(1, MAX_HISTORY_WALLPAPERS);
    NonZeroU16::new(limit).expect("history limit is clamped above zero")
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperSource {
    #[default]
    CurrentDesktop,
    RandomLibrary,
    RandomFavorites,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Light,
    Dark,
    #[default]
    System,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LanguageMode {
    #[default]
    System,
    English,
    SimplifiedChinese,
    German,
}

impl LanguageMode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
            Self::German => "de",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load_or_default(&self) -> Result<AppConfig> {
        if !self.path.exists() {
            let config = AppConfig::default();
            self.save(&config)?;
            return Ok(config);
        }

        let contents = fs::read_to_string(&self.path)
            .map_err(|source| SpotlitError::io(&self.path, source))?;

        match serde_json::from_str::<AppConfig>(&contents) {
            Ok(config) => {
                let normalized = config.clone().normalized();
                if normalized != config {
                    self.save(&normalized)?;
                }
                Ok(normalized)
            }
            Err(source) => {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %source,
                    "config file is invalid; backing it up and creating defaults"
                );
                backup_invalid_json(&self.path)?;
                let config = AppConfig::default();
                self.save(&config)?;
                Ok(config)
            }
        }
    }

    pub fn save(&self, config: &AppConfig) -> Result<()> {
        write_json_file(&self.path, config)
    }
}

pub(crate) fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SpotlitError::io(parent, source))?;
    }

    let tmp_path = path.with_extension("tmp");
    let serialized =
        serde_json::to_vec_pretty(value).map_err(|source| SpotlitError::JsonWrite {
            path: path.to_path_buf(),
            source,
        })?;

    {
        let mut file =
            fs::File::create(&tmp_path).map_err(|source| SpotlitError::io(&tmp_path, source))?;
        file.write_all(&serialized)
            .map_err(|source| SpotlitError::io(&tmp_path, source))?;
        file.write_all(b"\n")
            .map_err(|source| SpotlitError::io(&tmp_path, source))?;
    }

    if path.exists() {
        fs::remove_file(path).map_err(|source| SpotlitError::io(path, source))?;
    }

    fs::rename(&tmp_path, path).map_err(|source| SpotlitError::io(path, source))?;
    Ok(())
}

pub(crate) fn backup_invalid_json(path: &Path) -> Result<PathBuf> {
    let backup_path = invalid_backup_path(path);
    fs::rename(path, &backup_path).map_err(|source| SpotlitError::io(path, source))?;
    Ok(backup_path)
}

fn invalid_backup_path(path: &Path) -> PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("json");
    path.with_extension(format!("{extension}.invalid-{millis}"))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        ops::Deref,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        AppConfig, ConfigStore, DEFAULT_MAX_HISTORY_WALLPAPERS, LanguageMode, ThemeMode,
        WallpaperSource,
    };

    #[test]
    fn missing_config_fields_are_filled_from_defaults()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = temp_root("config-defaults");
        fs::create_dir_all(&root)?;
        let config_path = root.join("config.json");
        fs::write(&config_path, r#"{"auto_sync_lock_screen":true}"#)?;

        let config = ConfigStore::new(config_path).load_or_default()?;

        assert!(config.auto_sync_lock_screen);
        assert_eq!(config.wallpaper_source, WallpaperSource::CurrentDesktop);
        assert_eq!(config.sync_interval_minutes, 30);
        assert_eq!(config.theme, ThemeMode::System);
        assert!(!config.start_at_login);
        assert!(config.keep_running_in_background);
        assert!(config.automatic_update_checks);
        assert_eq!(config.language, LanguageMode::System);
        assert_eq!(
            config.max_history_wallpapers.map(|limit| limit.get()),
            Some(DEFAULT_MAX_HISTORY_WALLPAPERS)
        );

        Ok(())
    }

    #[test]
    fn legacy_platform_specific_config_fields_are_loaded()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = temp_root("config-legacy-fields");
        fs::create_dir_all(&root)?;
        let config_path = root.join("config.json");
        fs::write(
            &config_path,
            r#"{"lock_screen_source":"random_favorites","start_with_windows":true}"#,
        )?;

        let config = ConfigStore::new(config_path).load_or_default()?;

        assert_eq!(config.wallpaper_source, WallpaperSource::RandomFavorites);
        assert!(config.start_at_login);
        Ok(())
    }

    #[test]
    fn invalid_config_is_backed_up_and_replaced()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let root = temp_root("config-invalid");
        fs::create_dir_all(&root)?;
        let config_path = root.join("config.json");
        fs::write(&config_path, "{ invalid json")?;

        let config = ConfigStore::new(config_path.clone()).load_or_default()?;

        assert_eq!(config, AppConfig::default());
        assert_eq!(
            serde_json::from_str::<AppConfig>(&fs::read_to_string(&config_path)?)?,
            AppConfig::default()
        );
        assert!(
            fs::read_dir(&root)?
                .filter_map(std::result::Result::ok)
                .any(|entry| entry.file_name().to_string_lossy().contains(".invalid-"))
        );

        Ok(())
    }

    fn temp_root(name: &str) -> TempRoot {
        TempRoot::new(name)
    }

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let path =
                std::env::temp_dir().join(format!("spotlit-{name}-{}-{nanos}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            Self { path }
        }
    }

    impl Deref for TempRoot {
        type Target = Path;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    impl AsRef<Path> for TempRoot {
        fn as_ref(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
