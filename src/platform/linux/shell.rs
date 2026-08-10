use std::{
    path::Path,
    process::{Command, Stdio},
};

use crate::core::{Result, SpotlitError};

pub fn open_path(path: &Path) -> Result<()> {
    spawn_detached("xdg-open", [path.as_os_str()])
}

pub fn reveal_path(path: &Path) -> Result<()> {
    let folder = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    open_path(folder)
}

pub fn open_url_in_chrome(url: &str) -> Result<()> {
    for command in [
        "google-chrome",
        "google-chrome-stable",
        "chromium-browser",
        "chromium",
    ] {
        if spawn_detached(command, [std::ffi::OsStr::new(url)]).is_ok() {
            return Ok(());
        }
    }

    spawn_detached("xdg-open", [std::ffi::OsStr::new(url)])
}

fn spawn_detached<'a>(
    command: &str,
    args: impl IntoIterator<Item = &'a std::ffi::OsStr>,
) -> Result<()> {
    Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|source| SpotlitError::platform(format!("run {command}: {source}")))
}
