use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

use crate::core::{Result, SpotlitError};
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR},
        System::Registry::{
            HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegDeleteKeyValueW,
            RegGetValueW, RegSetKeyValueW,
        },
    },
    core::PCWSTR,
};

pub(crate) fn read_hkcu_dword(subkey: &str, name: &str) -> Result<Option<u32>> {
    let subkey = wide_null(subkey);
    let name = wide_null(name);
    let mut value = 0_u32;
    let mut value_size = size_of::<u32>() as u32;

    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut value_size),
        )
    };

    match status {
        ERROR_SUCCESS => Ok(Some(value)),
        ERROR_FILE_NOT_FOUND => Ok(None),
        error => Err(registry_error("read HKCU DWORD value", error)),
    }
}

pub(crate) fn read_hkcu_string(subkey: &str, name: &str) -> Result<Option<String>> {
    let subkey = wide_null(subkey);
    let name = wide_null(name);
    let mut byte_len = 0_u32;

    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut byte_len),
        )
    };

    match status {
        ERROR_SUCCESS => {}
        ERROR_FILE_NOT_FOUND => return Ok(None),
        error => return Err(registry_error("query HKCU string value size", error)),
    }

    let mut buffer = vec![0_u16; (byte_len as usize).div_ceil(2)];
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut byte_len),
        )
    };

    match status {
        ERROR_SUCCESS => {
            let len = buffer
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(buffer.len());
            Ok(Some(String::from_utf16_lossy(&buffer[..len])))
        }
        ERROR_FILE_NOT_FOUND => Ok(None),
        error => Err(registry_error("read HKCU string value", error)),
    }
}

pub(crate) fn write_hkcu_string(subkey: &str, name: &str, value: &str) -> Result<()> {
    let subkey = wide_null(subkey);
    let name = wide_null(name);
    let value = wide_null(value);
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            REG_SZ.0,
            Some(value.as_ptr().cast()),
            (value.len() * size_of::<u16>()) as u32,
        )
    };

    match status {
        ERROR_SUCCESS => Ok(()),
        error => Err(registry_error("write HKCU string value", error)),
    }
}

pub(crate) fn delete_hkcu_value(subkey: &str, name: &str) -> Result<()> {
    let subkey = wide_null(subkey);
    let name = wide_null(name);
    let status = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
        )
    };

    match status {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        error => Err(registry_error("delete HKCU value", error)),
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain([0]).collect()
}

fn registry_error(context: &str, error: WIN32_ERROR) -> SpotlitError {
    SpotlitError::platform(format!("{context}: Win32 error {}", error.0))
}
