use std::{env, path::PathBuf};

use crate::core::Result;

use super::registry::{delete_hkcu_value, read_hkcu_string, write_hkcu_string};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE_NAME: &str = "Spotlit";

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum StartupState {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone)]
pub struct StartupRegistration {
    app_name: String,
    command_path: PathBuf,
}

impl StartupRegistration {
    pub fn for_current_exe() -> Result<Self> {
        Ok(Self {
            app_name: RUN_VALUE_NAME.to_string(),
            command_path: env::current_exe().map_err(|source| {
                crate::core::SpotlitError::io("current executable path", source)
            })?,
        })
    }

    pub fn state(&self) -> Result<StartupState> {
        let Some(value) = read_hkcu_string(RUN_KEY, &self.app_name)? else {
            return Ok(StartupState::Disabled);
        };

        if value == self.command_line() || value == self.legacy_command_line() {
            Ok(StartupState::Enabled)
        } else {
            Ok(StartupState::Disabled)
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<StartupState> {
        if enabled {
            write_hkcu_string(RUN_KEY, &self.app_name, &self.command_line())?;
        } else {
            delete_hkcu_value(RUN_KEY, &self.app_name)?;
        }

        self.state()
    }

    fn command_line(&self) -> String {
        format!("\"{}\" --background", self.command_path.display())
    }

    fn legacy_command_line(&self) -> String {
        format!("\"{}\"", self.command_path.display())
    }
}
