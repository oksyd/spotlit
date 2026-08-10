use std::{
    ffi::OsString,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};

use crate::core::{Result, SpotlitError};
use windows::{
    Win32::{
        Foundation::HWND,
        UI::Controls::Dialogs::{
            CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER,
            OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT,
            OFN_PATHMUSTEXIST, OPENFILENAMEW,
        },
    },
    core::{PCWSTR, PWSTR},
};

const MAX_SELECTED_PATH_CHARS: usize = 32_768;

pub fn pick_wallpaper_image() -> Result<Option<PathBuf>> {
    let mut file_buffer = vec![0_u16; MAX_SELECTED_PATH_CHARS];
    let filter = wide_filter(&[
        (
            crate::i18n::file_dialog_images(),
            "*.jpg;*.jpeg;*.png;*.webp;*.bmp",
        ),
        (crate::i18n::file_dialog_all_files(), "*.*"),
    ]);
    let title = wide_nul(crate::i18n::import_wallpaper_dialog_title());

    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND::default(),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(file_buffer.as_mut_ptr()),
        nMaxFile: file_buffer.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_HIDEREADONLY
            | OFN_NOCHANGEDIR
            | OFN_PATHMUSTEXIST,
        ..Default::default()
    };

    let selected = unsafe { GetOpenFileNameW(&mut dialog).as_bool() };
    if selected {
        return Ok(selected_path(file_buffer));
    }

    let error = unsafe { CommDlgExtendedError() };
    if error.0 == 0 {
        Ok(None)
    } else {
        Err(SpotlitError::platform(format!(
            "file open dialog failed with common dialog error 0x{:x}",
            error.0
        )))
    }
}

pub fn pick_export_image_path(default_file_name: &str) -> Result<Option<PathBuf>> {
    let mut file_buffer = file_buffer_with_default(default_file_name);
    let filter = wide_filter(&[
        (
            crate::i18n::file_dialog_images(),
            "*.jpg;*.jpeg;*.png;*.webp;*.bmp",
        ),
        (crate::i18n::file_dialog_all_files(), "*.*"),
    ]);
    let title = wide_nul(crate::i18n::export_wallpaper_dialog_title());
    let default_extension = wide_nul(default_extension(default_file_name));

    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND::default(),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        nFilterIndex: 1,
        lpstrFile: PWSTR(file_buffer.as_mut_ptr()),
        nMaxFile: file_buffer.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        lpstrDefExt: PCWSTR(default_extension.as_ptr()),
        Flags: OFN_EXPLORER | OFN_HIDEREADONLY | OFN_NOCHANGEDIR | OFN_OVERWRITEPROMPT,
        ..Default::default()
    };

    let selected = unsafe { GetSaveFileNameW(&mut dialog).as_bool() };
    if selected {
        return Ok(selected_path(file_buffer));
    }

    let error = unsafe { CommDlgExtendedError() };
    if error.0 == 0 {
        Ok(None)
    } else {
        Err(SpotlitError::platform(format!(
            "file save dialog failed with common dialog error 0x{:x}",
            error.0
        )))
    }
}

fn selected_path(buffer: Vec<u16>) -> Option<PathBuf> {
    let nul = buffer.iter().position(|value| *value == 0)?;
    if nul == 0 {
        return None;
    }

    Some(PathBuf::from(OsString::from_wide(&buffer[..nul])))
}

fn wide_nul(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_filter(pairs: &[(&str, &str)]) -> Vec<u16> {
    let mut encoded = Vec::new();
    for (label, pattern) in pairs {
        encoded.extend(label.encode_utf16());
        encoded.push(0);
        encoded.extend(pattern.encode_utf16());
        encoded.push(0);
    }
    encoded.push(0);
    encoded
}

fn file_buffer_with_default(default_file_name: &str) -> Vec<u16> {
    let mut buffer = vec![0_u16; MAX_SELECTED_PATH_CHARS];
    for (index, value) in default_file_name
        .encode_utf16()
        .take(MAX_SELECTED_PATH_CHARS - 1)
        .enumerate()
    {
        buffer[index] = value;
    }
    buffer
}

fn default_extension(default_file_name: &str) -> &str {
    Path::new(default_file_name)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("jpg")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_filter_uses_double_nul_terminator() {
        let filter = wide_filter(&[("Images", "*.jpg"), ("All", "*.*")]);
        assert_eq!(filter.last(), Some(&0));
        assert_eq!(filter.get(filter.len() - 2), Some(&0));
    }

    #[test]
    fn selected_path_returns_none_for_empty_selection() {
        assert!(selected_path(vec![0, 0, 0]).is_none());
    }

    #[test]
    fn file_buffer_with_default_keeps_default_name() {
        let buffer = file_buffer_with_default("wallpaper.png");
        assert_eq!(
            selected_path(buffer).as_deref(),
            Some(Path::new("wallpaper.png"))
        );
    }

    #[test]
    fn default_extension_uses_file_name_extension_or_jpg() {
        assert_eq!(default_extension("wallpaper.png"), "png");
        assert_eq!(default_extension("wallpaper"), "jpg");
    }
}
