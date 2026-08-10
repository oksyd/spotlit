use std::process::Command;

use crate::core::{Result, SpotlitError};

pub fn lock_workstation() -> Result<()> {
    let output = Command::new("loginctl")
        .arg("lock-session")
        .output()
        .map_err(|source| SpotlitError::platform(format!("run loginctl: {source}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(SpotlitError::platform(if stderr.is_empty() {
        "loginctl lock-session failed".to_string()
    } else {
        format!("loginctl lock-session failed: {stderr}")
    }))
}
