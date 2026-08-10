use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

use crate::core::{Result, SpotlitError};
use windows::Win32::UI::WindowsAndMessaging::{
    SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

pub fn current_desktop_wallpaper() -> Result<Option<PathBuf>> {
    let mut buffer = vec![0_u16; 32_768];

    unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buffer.len() as u32,
            Some(buffer.as_mut_ptr().cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .map_err(SpotlitError::platform)?;
    }

    let nul = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    if nul == 0 {
        return Ok(None);
    }

    Ok(Some(PathBuf::from(OsString::from_wide(&buffer[..nul]))))
}
