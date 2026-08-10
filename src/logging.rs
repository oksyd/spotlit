use std::{fs::OpenOptions, path::Path, sync::Mutex};

use anyhow::Context;

const LOG_FILE_NAME: &str = "spotlit.log";
const LOG_BACKUP_FILE_NAME: &str = "spotlit.log.1";
const MAX_LOG_FILE_BYTES: u64 = 1024 * 1024;

pub(crate) fn init_tracing(log_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(log_dir).context("failed to create log directory")?;
    let log_path = log_dir.join(LOG_FILE_NAME);
    rotate_log_file(&log_path, MAX_LOG_FILE_BYTES).context("failed to rotate spotlit log file")?;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "spotlit=info".into());
    let log_file = match OpenOptions::new().create(true).append(true).open(&log_path) {
        Ok(log_file) => log_file,
        Err(error) => {
            eprintln!(
                "spotlit: failed to open log file at {}; falling back to stderr: {error}",
                log_path.display()
            );
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(std::io::stderr)
                .try_init()
                .map_err(|error| {
                    anyhow::anyhow!("failed to install tracing subscriber: {error}")
                })?;
            return Ok(());
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Mutex::new(log_file))
        .try_init()
        .map_err(|error| anyhow::anyhow!("failed to install tracing subscriber: {error}"))?;
    Ok(())
}

fn rotate_log_file(log_path: &Path, max_bytes: u64) -> anyhow::Result<()> {
    let metadata = match std::fs::metadata(log_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("failed to inspect current log file"),
    };

    if metadata.len() <= max_bytes {
        return Ok(());
    }

    let backup_path = log_path.with_file_name(LOG_BACKUP_FILE_NAME);
    match std::fs::remove_file(&backup_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("failed to remove old log backup"),
    }

    std::fs::rename(log_path, &backup_path).context("failed to rotate current log file")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        ops::Deref,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{LOG_BACKUP_FILE_NAME, LOG_FILE_NAME, rotate_log_file};

    #[test]
    fn rotates_log_file_when_it_exceeds_limit() -> anyhow::Result<()> {
        let root = temp_root("log-rotate");
        fs::create_dir_all(&root)?;
        let log_path = root.join(LOG_FILE_NAME);
        let backup_path = root.join(LOG_BACKUP_FILE_NAME);
        fs::write(&log_path, b"abcdef")?;

        rotate_log_file(&log_path, 5)?;

        assert!(!log_path.exists());
        assert_eq!(fs::read(&backup_path)?, b"abcdef");

        Ok(())
    }

    #[test]
    fn keeps_log_file_when_it_is_within_limit() -> anyhow::Result<()> {
        let root = temp_root("log-keep");
        fs::create_dir_all(&root)?;
        let log_path = root.join(LOG_FILE_NAME);
        fs::write(&log_path, b"abc")?;

        rotate_log_file(&log_path, 5)?;

        assert_eq!(fs::read(&log_path)?, b"abc");
        assert!(!root.join(LOG_BACKUP_FILE_NAME).exists());

        Ok(())
    }

    fn temp_root(name: &str) -> TempRoot {
        TempRoot::new(name)
    }

    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let path =
                std::env::temp_dir().join(format!("spotlit-{name}-{}-{nanos}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            Self { path }
        }
    }

    impl Deref for TempRoot {
        type Target = Path;

        fn deref(&self) -> &Self::Target {
            &self.path
        }
    }

    impl AsRef<Path> for TempRoot {
        fn as_ref(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
