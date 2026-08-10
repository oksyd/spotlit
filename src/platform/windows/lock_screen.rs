use std::{fs, path::Path};

use crate::core::{Result, SpotlitError};
use windows::{
    Storage::StorageFile,
    System::UserProfile::{LockScreen, UserProfilePersonalizationSettings},
    Win32::{
        Foundation::{
            APPMODEL_ERROR_NO_PACKAGE, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, WIN32_ERROR,
        },
        Storage::Packaging::Appx::GetCurrentPackageFullName,
    },
    core::{HSTRING, PWSTR},
};

#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsLockScreen;

impl WindowsLockScreen {
    pub fn set_lock_screen_wallpaper(&self, image_path: &Path) -> Result<()> {
        set_lock_screen_wallpaper(image_path)
    }
}

pub fn set_lock_screen_wallpaper(image_path: &Path) -> Result<()> {
    let diagnostics = LockScreenImageDiagnostics::inspect(image_path);
    let package_identity = current_package_identity();
    tracing::info!(
        image_path = %image_path.display(),
        image_extension = %diagnostics.extension,
        image_exists = diagnostics.exists,
        image_is_file = diagnostics.is_file,
        image_bytes = ?diagnostics.bytes,
        package_identity = package_identity.kind(),
        package_full_name = ?package_identity.full_name(),
        "windows lock screen image update requested"
    );

    let settings = match UserProfilePersonalizationSettings::Current() {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(
                image_path = %image_path.display(),
                error = %error,
                "failed to open Windows lock screen personalization settings"
            );
            return Err(SpotlitError::platform(error));
        }
    };

    let supported = match UserProfilePersonalizationSettings::IsSupported() {
        Ok(supported) => supported,
        Err(error) => {
            tracing::warn!(
                image_path = %image_path.display(),
                error = %error,
                "failed to query Windows lock screen personalization support"
            );
            return Err(SpotlitError::platform(error));
        }
    };
    tracing::info!(
        image_path = %image_path.display(),
        supported,
        "windows lock screen personalization support checked"
    );
    if !supported {
        tracing::warn!(
            image_path = %image_path.display(),
            "Windows reports lock screen personalization is unsupported"
        );
        return Err(SpotlitError::platform(
            "lock screen personalization is not supported for this account or OS policy",
        ));
    }

    let path = HSTRING::from(image_path.to_string_lossy().as_ref());
    let file_operation = match StorageFile::GetFileFromPathAsync(&path) {
        Ok(operation) => operation,
        Err(error) => {
            tracing::warn!(
                image_path = %image_path.display(),
                error = %error,
                "failed to start Windows storage file lookup"
            );
            return Err(SpotlitError::platform(error));
        }
    };
    let file = match file_operation.join() {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(
                image_path = %image_path.display(),
                error = %error,
                "failed to resolve Windows storage file for lock screen image"
            );
            return Err(SpotlitError::platform(error));
        }
    };
    tracing::debug!(
        image_path = %image_path.display(),
        "Windows storage file resolved for lock screen image"
    );

    let change_operation = match settings.TrySetLockScreenImageAsync(&file) {
        Ok(operation) => operation,
        Err(error) => {
            tracing::warn!(
                image_path = %image_path.display(),
                error = %error,
                "failed to start Windows lock screen image update"
            );
            return Err(SpotlitError::platform(error));
        }
    };
    let changed = match change_operation.join() {
        Ok(changed) => changed,
        Err(error) => {
            tracing::warn!(
                image_path = %image_path.display(),
                error = %error,
                "Windows lock screen image update failed"
            );
            return Err(SpotlitError::platform(error));
        }
    };
    tracing::info!(
        image_path = %image_path.display(),
        changed,
        "windows lock screen image update completed"
    );

    if changed {
        return Ok(());
    }

    tracing::warn!(
        image_path = %image_path.display(),
        image_extension = %diagnostics.extension,
        image_exists = diagnostics.exists,
        image_is_file = diagnostics.is_file,
        image_bytes = ?diagnostics.bytes,
        supported,
        changed = false,
        package_identity = package_identity.kind(),
        package_full_name = ?package_identity.full_name(),
        "Windows rejected the lock screen image change; trying LockScreen.SetImageFileAsync fallback"
    );

    set_lock_screen_with_legacy_api(&file, image_path, &package_identity)
}

fn set_lock_screen_with_legacy_api(
    file: &StorageFile,
    image_path: &Path,
    package_identity: &PackageIdentity,
) -> Result<()> {
    let operation = match LockScreen::SetImageFileAsync(file) {
        Ok(operation) => operation,
        Err(error) => {
            tracing::warn!(
                image_path = %image_path.display(),
                package_identity = package_identity.kind(),
                package_full_name = ?package_identity.full_name(),
                error = %error,
                "failed to start LockScreen.SetImageFileAsync fallback"
            );
            return Err(SpotlitError::platform(format!(
                "Windows rejected the lock screen image change; LockScreen.SetImageFileAsync failed to start ({identity}): {error}",
                identity = package_identity.description(),
            )));
        }
    };

    match operation.join() {
        Ok(()) => {
            tracing::info!(
                image_path = %image_path.display(),
                package_identity = package_identity.kind(),
                package_full_name = ?package_identity.full_name(),
                "LockScreen.SetImageFileAsync fallback applied"
            );
            Ok(())
        }
        Err(error) => {
            tracing::warn!(
                image_path = %image_path.display(),
                package_identity = package_identity.kind(),
                package_full_name = ?package_identity.full_name(),
                error = %error,
                "LockScreen.SetImageFileAsync fallback failed"
            );
            Err(SpotlitError::platform(format!(
                "Windows rejected the lock screen image change; LockScreen.SetImageFileAsync failed ({identity}): {error}",
                identity = package_identity.description(),
            )))
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum PackageIdentity {
    Packaged(String),
    Unpackaged,
    Unknown { code: u32 },
}

impl PackageIdentity {
    fn kind(&self) -> &'static str {
        match self {
            Self::Packaged(_) => "packaged",
            Self::Unpackaged => "unpackaged",
            Self::Unknown { .. } => "unknown",
        }
    }

    fn full_name(&self) -> Option<&str> {
        match self {
            Self::Packaged(full_name) => Some(full_name),
            Self::Unpackaged | Self::Unknown { .. } => None,
        }
    }

    fn description(&self) -> String {
        match self {
            Self::Packaged(full_name) => format!("packaged: {full_name}"),
            Self::Unpackaged => "unpackaged process".to_string(),
            Self::Unknown { code } => format!("package identity unknown: {code}"),
        }
    }
}

fn current_package_identity() -> PackageIdentity {
    let mut length = 0;
    let status = unsafe { GetCurrentPackageFullName(&mut length, None) };

    if status == APPMODEL_ERROR_NO_PACKAGE {
        return PackageIdentity::Unpackaged;
    }

    if status != ERROR_INSUFFICIENT_BUFFER && status != ERROR_SUCCESS {
        return PackageIdentity::Unknown { code: status.0 };
    }

    if length == 0 {
        return PackageIdentity::Unknown { code: status.0 };
    }

    let mut buffer = vec![0u16; length as usize];
    let status =
        unsafe { GetCurrentPackageFullName(&mut length, Some(PWSTR(buffer.as_mut_ptr()))) };
    if status != ERROR_SUCCESS {
        return package_identity_from_error(status);
    }

    let end = buffer
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(buffer.len());
    PackageIdentity::Packaged(String::from_utf16_lossy(&buffer[..end]))
}

fn package_identity_from_error(error: WIN32_ERROR) -> PackageIdentity {
    if error == APPMODEL_ERROR_NO_PACKAGE {
        PackageIdentity::Unpackaged
    } else {
        PackageIdentity::Unknown { code: error.0 }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct LockScreenImageDiagnostics {
    extension: String,
    exists: bool,
    is_file: bool,
    bytes: Option<u64>,
}

impl LockScreenImageDiagnostics {
    fn inspect(path: &Path) -> Self {
        let metadata = fs::metadata(path).ok();
        let is_file = metadata.as_ref().is_some_and(|metadata| metadata.is_file());

        Self {
            extension: path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase(),
            exists: metadata.is_some(),
            is_file,
            bytes: metadata
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len()),
        }
    }
}
