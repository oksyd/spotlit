use std::{env, path::PathBuf};

use crate::core::{AppPaths, Result, SpotlitError};

const APP_DIR_NAME: &str = "Spotlit";
const DATA_DIR_OVERRIDE_ENV: &str = "SPOTLIT_DATA_DIR";
const LOCAL_APP_DATA_ENV: &str = "LOCALAPPDATA";

pub fn app_paths() -> Result<AppPaths> {
    if let Some(data_dir) = app_data_dir_override() {
        return Ok(AppPaths::new(data_dir));
    }

    app_local_data_dir()
        .map(AppPaths::new)
        .ok_or_else(|| SpotlitError::platform("LOCALAPPDATA is not set"))
}

fn app_data_dir_override() -> Option<PathBuf> {
    env_path(DATA_DIR_OVERRIDE_ENV)
}

fn app_local_data_dir() -> Option<PathBuf> {
    env_path(LOCAL_APP_DATA_ENV).map(|path| path.join(APP_DIR_NAME))
}

fn env_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os(name).map(PathBuf::from)?;
    (!path.as_os_str().is_empty()).then_some(path)
}

pub fn spotlight_source_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(local_appdata) = env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        dirs.push(
            local_appdata
                .join("Packages")
                .join("Microsoft.Windows.ContentDeliveryManager_cw5n1h2txyewy")
                .join("LocalState")
                .join("Assets"),
        );
        dirs.push(
            local_appdata
                .join("Microsoft")
                .join("Windows")
                .join("Themes")
                .join("CachedFiles"),
        );
        dirs.push(
            local_appdata
                .join("Microsoft")
                .join("Windows")
                .join("Themes")
                .join("RoamedThemeFiles")
                .join("DesktopBackground"),
        );
    }

    if let Some(appdata) = env::var_os("APPDATA").map(PathBuf::from) {
        dirs.push(
            appdata
                .join("Microsoft")
                .join("Windows")
                .join("Themes")
                .join("CachedFiles"),
        );
    }

    dirs
}
