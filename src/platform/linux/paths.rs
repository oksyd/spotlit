use std::{env, path::PathBuf};

use crate::core::{AppPaths, Result, SpotlitError};

const APP_DIR_NAME: &str = "Spotlit";
const DATA_DIR_OVERRIDE_ENV: &str = "SPOTLIT_DATA_DIR";
const XDG_DATA_HOME_ENV: &str = "XDG_DATA_HOME";
const HOME_ENV: &str = "HOME";

pub fn app_paths() -> Result<AppPaths> {
    if let Some(data_dir) = env_path(DATA_DIR_OVERRIDE_ENV) {
        return Ok(AppPaths::new(data_dir));
    }

    if let Some(data_home) = env_path(XDG_DATA_HOME_ENV) {
        return Ok(AppPaths::new(data_home.join(APP_DIR_NAME)));
    }

    if let Some(home) = env_path(HOME_ENV) {
        return Ok(AppPaths::new(
            home.join(".local").join("share").join(APP_DIR_NAME),
        ));
    }

    Err(SpotlitError::platform(
        "neither SPOTLIT_DATA_DIR, XDG_DATA_HOME, nor HOME is set",
    ))
}

pub fn wallpaper_source_dirs(paths: &AppPaths) -> Vec<PathBuf> {
    vec![super::bing_wallpaper_dir(paths)]
}

fn env_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os(name).map(PathBuf::from)?;
    (!path.as_os_str().is_empty()).then_some(path)
}
