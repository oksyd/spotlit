use std::process::Command;

use crate::core::Result;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SystemTheme {
    Light,
    Dark,
}

pub fn system_theme() -> Result<SystemTheme> {
    let output = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output();

    let Ok(output) = output else {
        return Ok(SystemTheme::Light);
    };

    if !output.status.success() {
        return Ok(SystemTheme::Light);
    }

    let value = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    if value.contains("dark") {
        Ok(SystemTheme::Dark)
    } else {
        Ok(SystemTheme::Light)
    }
}
