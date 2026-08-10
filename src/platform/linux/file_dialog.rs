use std::{
    path::{Path, PathBuf},
    process::Command,
};

use crate::core::{Result, SpotlitError};

pub fn pick_wallpaper_image() -> Result<Option<PathBuf>> {
    let title = format!("--title={}", crate::i18n::import_wallpaper_dialog_title());
    let filter = format!(
        "--file-filter={} | *.jpg *.jpeg *.png *.webp",
        crate::i18n::file_dialog_images()
    );
    let output = Command::new("zenity")
        .args(["--file-selection", title.as_str(), filter.as_str()])
        .output()
        .map_err(|source| SpotlitError::platform(format!("run zenity: {source}")))?;

    dialog_path(output)
}

pub fn pick_export_image_path(default_file_name: &str) -> Result<Option<PathBuf>> {
    let title = format!("--title={}", crate::i18n::export_wallpaper_dialog_title());
    let output = Command::new("zenity")
        .args([
            "--file-selection",
            "--save",
            "--confirm-overwrite",
            title.as_str(),
            "--filename",
            default_file_name,
        ])
        .output()
        .map_err(|source| SpotlitError::platform(format!("run zenity: {source}")))?;

    dialog_path(output)
}

fn dialog_path(output: std::process::Output) -> Result<Option<PathBuf>> {
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Ok((!path.is_empty()).then(|| Path::new(&path).to_path_buf()));
    }

    if output.status.code() == Some(1) {
        return Ok(None);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(SpotlitError::platform(if stderr.is_empty() {
        "file dialog failed".to_string()
    } else {
        format!("file dialog failed: {stderr}")
    }))
}
