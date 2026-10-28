use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::{Result, Wallpaper, config::write_json_file};

#[derive(Serialize)]
struct LockScreenInfo<'a> {
    version: u8,
    image: String,
    wallpaper_id: &'a str,
    title: Option<String>,
    description: Option<String>,
    credit: Option<String>,
}

pub(super) fn info_path(image: &Path) -> PathBuf {
    image.with_added_extension("spotlit.json")
}

pub(super) fn image_for_info(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?.strip_suffix(".spotlit.json")?;
    Some(path.with_file_name(name))
}

pub(super) fn write_info(image: &Path, wallpaper: &Wallpaper) -> Result<()> {
    let metadata = &wallpaper.spotlight;
    let title = display_text(metadata.title.as_deref(), 200);
    let description = display_text(metadata.caption.as_deref(), 600)
        .filter(|value| Some(value) != title.as_ref());
    let credit = display_text(metadata.copyright.as_deref(), 600)
        .filter(|value| Some(value) != title.as_ref() && Some(value) != description.as_ref());
    if title.is_none() && description.is_none() && credit.is_none() {
        return Ok(());
    }

    write_json_file(
        &info_path(image),
        &LockScreenInfo {
            version: 1,
            image: image
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            wallpaper_id: wallpaper.id.as_str(),
            title,
            description,
            credit,
        },
    )
}

fn display_text(value: Option<&str>, max_chars: usize) -> Option<String> {
    let text: String = value?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect();
    (!text.is_empty()).then_some(text)
}
