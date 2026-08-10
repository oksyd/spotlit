use std::{
    env, fs,
    io::{self, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    thread,
};

use crate::core::{Result, SpotlitError};

const XDG_RUNTIME_DIR_ENV: &str = "XDG_RUNTIME_DIR";
const USER_ENV: &str = "USER";
const UID_ENV: &str = "UID";
const SOCKET_FILE_NAME: &str = "spotlit.sock";
const ACTIVATION_MESSAGE: &[u8] = b"activate\n";
const ACTIVATION_THREAD_STACK_SIZE: usize = 128 * 1024;

#[derive(Debug)]
pub struct SingleInstanceGuard {
    socket_path: PathBuf,
    listener: UnixListener,
}

impl SingleInstanceGuard {
    pub fn acquire() -> Result<Option<Self>> {
        let socket_path = activation_socket_path();

        if signal_existing_instance(&socket_path).is_ok() {
            return Ok(None);
        }

        remove_stale_socket(&socket_path)?;
        let listener = bind_activation_socket(&socket_path)?;
        Ok(Some(Self {
            socket_path,
            listener,
        }))
    }

    pub fn start_activation_listener<F>(&self, on_activate: F) -> Result<()>
    where
        F: Fn() + Send + Sync + 'static,
    {
        let listener = self
            .listener
            .try_clone()
            .map_err(|source| SpotlitError::io(&self.socket_path, source))?;

        thread::Builder::new()
            .name("spotlit-activation-listener".to_string())
            .stack_size(ACTIVATION_THREAD_STACK_SIZE)
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(_) => on_activate(),
                        Err(error) => {
                            tracing::warn!(%error, "activation socket accept failed");
                            break;
                        }
                    }
                }
            })
            .map(|_| ())
            .map_err(|source| SpotlitError::io("activation listener thread", source))
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.socket_path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                error = %error,
                path = %self.socket_path.display(),
                "failed to remove activation socket"
            );
        }
    }
}

fn activation_socket_path() -> PathBuf {
    if let Some(runtime_dir) = env_path(XDG_RUNTIME_DIR_ENV) {
        return runtime_dir.join(SOCKET_FILE_NAME);
    }

    env::temp_dir().join(format!("spotlit-{}.sock", user_token()))
}

fn user_token() -> String {
    env::var(UID_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env::var(USER_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "user".to_string())
}

fn env_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os(name).map(PathBuf::from)?;
    (!path.as_os_str().is_empty()).then_some(path)
}

fn bind_activation_socket(socket_path: &Path) -> Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).map_err(|source| SpotlitError::io(parent, source))?;
    }

    UnixListener::bind(socket_path).map_err(|source| SpotlitError::io(socket_path, source))
}

fn signal_existing_instance(socket_path: &Path) -> Result<()> {
    let mut stream =
        UnixStream::connect(socket_path).map_err(|source| SpotlitError::io(socket_path, source))?;
    stream
        .write_all(ACTIVATION_MESSAGE)
        .map_err(|source| SpotlitError::io(socket_path, source))
}

fn remove_stale_socket(socket_path: &Path) -> Result<()> {
    match fs::remove_file(socket_path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SpotlitError::io(socket_path, source)),
    }
}
