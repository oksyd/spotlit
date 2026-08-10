use std::{
    env,
    path::Path,
    process::{Command, Stdio},
};

use crate::core::{Result, SpotlitError};

pub fn open_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(SpotlitError::platform(format!(
            "path does not exist: {}",
            path.display()
        )));
    }

    Command::new("explorer.exe")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| SpotlitError::io(path, source))?;
    Ok(())
}

pub fn reveal_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(SpotlitError::platform(format!(
            "path does not exist: {}",
            path.display()
        )));
    }

    Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| SpotlitError::io(path, source))?;
    Ok(())
}

pub fn open_url_in_chrome(url: &str) -> Result<()> {
    let url = validated_web_url(url)?;

    for candidate in chrome_candidates() {
        let mut command = Command::new(&candidate);
        command.arg(url);
        if spawn_detached(&mut command).is_ok() {
            return Ok(());
        }
    }

    let mut command = Command::new("explorer.exe");
    command.arg(url);
    spawn_detached(&mut command)
        .map_err(|source| SpotlitError::platform(format!("open URL in browser: {source}")))?;
    Ok(())
}

fn validated_web_url(url: &str) -> Result<&str> {
    let url = url.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        return Ok(url);
    }

    Err(SpotlitError::platform(format!(
        "unsupported wallpaper info URL: {url}"
    )))
}

fn chrome_candidates() -> Vec<String> {
    let mut candidates = vec!["chrome.exe".to_string()];

    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
        let Ok(root) = env::var(variable) else {
            continue;
        };
        candidates.push(
            Path::new(&root)
                .join("Google")
                .join("Chrome")
                .join("Application")
                .join("chrome.exe")
                .to_string_lossy()
                .into_owned(),
        );
    }

    candidates
}

fn spawn_detached(command: &mut Command) -> std::io::Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}
