use std::{
    fs,
    path::{Path, PathBuf},
};

use image::ImageReader;

use crate::core::{Result, SpotlitError};

const THUMBNAIL_MAX_WIDTH: u32 = 960;
const THUMBNAIL_MAX_HEIGHT: u32 = 540;

pub fn ensure_thumbnail(image_path: &Path, thumbnail_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(thumbnail_dir).map_err(|source| SpotlitError::io(thumbnail_dir, source))?;

    let thumbnail_path = thumbnail_path_for_image(image_path, thumbnail_dir);

    if thumbnail_path.exists() {
        return Ok(thumbnail_path);
    }

    let image = ImageReader::open(image_path)
        .map_err(|source| SpotlitError::io(image_path, source))?
        .with_guessed_format()
        .map_err(|source| SpotlitError::io(image_path, source))?
        .decode()
        .map_err(|source| SpotlitError::image(image_path, source))?;

    let thumbnail = image.thumbnail(THUMBNAIL_MAX_WIDTH, THUMBNAIL_MAX_HEIGHT);
    thumbnail
        .save_with_format(&thumbnail_path, image::ImageFormat::Jpeg)
        .map_err(|source| SpotlitError::image(&thumbnail_path, source))?;

    Ok(thumbnail_path)
}

pub fn thumbnail_path_for_image(image_path: &Path, thumbnail_dir: &Path) -> PathBuf {
    let stem = image_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("wallpaper");
    thumbnail_dir.join(format!("{stem}.jpg"))
}
