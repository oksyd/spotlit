use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

use crate::core::{Result, SpotlitError};

use super::gnome_extension::ensure_extension_enabled;

const DESKTOP_BACKGROUND_SCHEMA: &str = "org.gnome.desktop.background";
const LOCK_SCREEN_BACKGROUND_SCHEMA: &str = "org.gnome.desktop.screensaver";

#[derive(Debug, Clone, Copy, Default)]
pub struct GnomeLockScreen;

impl GnomeLockScreen {
    pub fn set_lock_screen_wallpaper(&self, image_path: &Path) -> Result<()> {
        ensure_extension_enabled()?;
        let uri = file_uri(image_path)?;
        gsettings_set(LOCK_SCREEN_BACKGROUND_SCHEMA, "picture-uri", &uri)
    }
}

pub fn current_desktop_wallpaper() -> Result<Option<PathBuf>> {
    let uri = gsettings_get(DESKTOP_BACKGROUND_SCHEMA, "picture-uri-dark")
        .ok()
        .and_then(|value| parse_gsettings_string(&value))
        .or_else(|| {
            gsettings_get(DESKTOP_BACKGROUND_SCHEMA, "picture-uri")
                .ok()
                .and_then(|value| parse_gsettings_string(&value))
        });

    Ok(uri.as_deref().and_then(file_uri_to_path))
}

fn gsettings_get(schema: &str, key: &str) -> Result<String> {
    let output = Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .map_err(|source| SpotlitError::platform(format!("run gsettings: {source}")))?;

    if !output.status.success() {
        return Err(command_error("gsettings get", &output.stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn gsettings_set(schema: &str, key: &str, value: &str) -> Result<()> {
    let output = Command::new("gsettings")
        .args(["set", schema, key, value])
        .output()
        .map_err(|source| SpotlitError::platform(format!("run gsettings: {source}")))?;

    if !output.status.success() {
        return Err(command_error("gsettings set", &output.stderr));
    }

    Ok(())
}

fn command_error(command: &str, stderr: &[u8]) -> SpotlitError {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if stderr.is_empty() {
        SpotlitError::platform(format!("{command} failed"))
    } else {
        SpotlitError::platform(format!("{command} failed: {stderr}"))
    }
}

fn parse_gsettings_string(value: &str) -> Option<String> {
    let value = value.trim();
    if value == "''" || value == "\"\"" {
        return None;
    }

    if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    {
        return Some(inner.replace("\\'", "'"));
    }

    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        return Some(inner.replace("\\\"", "\""));
    }

    (!value.is_empty()).then(|| value.to_string())
}

fn file_uri(path: &Path) -> Result<String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| {
                SpotlitError::platform(format!("resolve current directory: {source}"))
            })?
            .join(path)
    };

    Ok(format!("file://{}", percent_encode_os(path.as_os_str())))
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let path = uri.strip_prefix("file://")?;
    percent_decode_path(path).map(PathBuf::from)
}

#[cfg(target_os = "linux")]
fn percent_encode_os(value: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;

    percent_encode_bytes(value.as_bytes())
}

fn percent_encode_bytes(bytes: &[u8]) -> String {
    let mut encoded = String::new();
    for byte in bytes {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            byte => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn percent_decode_path(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex_pair(high, low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8(decoded).ok()
}

fn hex_pair(high: u8, low: u8) -> Option<u8> {
    Some(hex_digit(high)? << 4 | hex_digit(low)?)
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{file_uri, file_uri_to_path};

    #[test]
    fn wallpaper_paths_round_trip_through_file_uris() {
        let path = Path::new("/home/user/Wallpapers/Lake Tahoe #1.jpg");
        let uri = file_uri(path).expect("absolute path should produce a URI");

        assert_eq!(uri, "file:///home/user/Wallpapers/Lake%20Tahoe%20%231.jpg");
        assert_eq!(file_uri_to_path(&uri).as_deref(), Some(path));
    }
}
