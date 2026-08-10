use std::{fs, path::Path};

use crate::core::{Result, SpotlitError};

pub(crate) fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SpotlitError::io(path, source)),
    }
}
