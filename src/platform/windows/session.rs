use crate::core::{Result, SpotlitError};
use windows::Win32::System::Shutdown::LockWorkStation;

pub fn lock_workstation() -> Result<()> {
    unsafe { LockWorkStation() }.map_err(SpotlitError::platform)
}
