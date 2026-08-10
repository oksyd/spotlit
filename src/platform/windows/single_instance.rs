use std::{thread, time::Duration};

use crate::core::{Result, SpotlitError};
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0},
        System::Threading::{
            CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
            WaitForSingleObject,
        },
    },
    core::HSTRING,
};

const INSTANCE_MUTEX_NAME: &str = "Local\\SpotlitDesktopWallpaperTool";
const ACTIVATION_EVENT_NAME: &str = "Local\\SpotlitDesktopWallpaperTool.Activate";
const ACTIVATION_THREAD_STACK_SIZE: usize = 128 * 1024;
const ACTIVATION_SIGNAL_RETRIES: usize = 4;
const ACTIVATION_SIGNAL_RETRY_DELAY: Duration = Duration::from_millis(25);

#[derive(Debug)]
pub struct SingleInstanceGuard {
    mutex: HANDLE,
    activation_event: HANDLE,
}

impl SingleInstanceGuard {
    pub fn acquire() -> Result<Option<Self>> {
        let name = HSTRING::from(INSTANCE_MUTEX_NAME);
        let mutex = unsafe { CreateMutexW(None, true, &name) }.map_err(SpotlitError::platform)?;

        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe {
                CloseHandle(mutex).map_err(SpotlitError::platform)?;
            }
            if let Err(error) = signal_existing_instance() {
                tracing::warn!(error = %error, "failed to signal running spotlit instance");
            }
            return Ok(None);
        }

        let activation_event = create_activation_event()?;

        Ok(Some(Self {
            mutex,
            activation_event,
        }))
    }

    pub fn start_activation_listener<F>(&self, on_activate: F) -> Result<()>
    where
        F: Fn() + Send + Sync + 'static,
    {
        let activation_event = self.activation_event.0 as usize;
        std::thread::Builder::new()
            .name("spotlit-activation-listener".to_string())
            .stack_size(ACTIVATION_THREAD_STACK_SIZE)
            .spawn(move || {
                let activation_event = HANDLE(activation_event as *mut std::ffi::c_void);
                loop {
                    let wait = unsafe { WaitForSingleObject(activation_event, INFINITE) };
                    if wait == WAIT_OBJECT_0 {
                        on_activate();
                    } else {
                        tracing::warn!(wait = wait.0, "activation event wait failed");
                        break;
                    }
                }
            })
            .map(|_| ())
            .map_err(|source| SpotlitError::io("activation listener thread", source))
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        if let Err(error) = unsafe { CloseHandle(self.activation_event) } {
            tracing::warn!(error = %error, "failed to close activation event");
        }
        if let Err(error) = unsafe { CloseHandle(self.mutex) } {
            tracing::warn!(error = %error, "failed to close single instance mutex");
        }
    }
}

fn create_activation_event() -> Result<HANDLE> {
    let name = HSTRING::from(ACTIVATION_EVENT_NAME);
    unsafe { CreateEventW(None, false, false, &name) }.map_err(SpotlitError::platform)
}

fn signal_existing_instance() -> Result<()> {
    let name = HSTRING::from(ACTIVATION_EVENT_NAME);
    let event = open_activation_event_with_retry(&name)?;
    unsafe {
        SetEvent(event).map_err(SpotlitError::platform)?;
        CloseHandle(event).map_err(SpotlitError::platform)?;
    }
    Ok(())
}

fn open_activation_event_with_retry(name: &HSTRING) -> Result<HANDLE> {
    let mut last_error = None;

    for attempt in 0..=ACTIVATION_SIGNAL_RETRIES {
        match unsafe { OpenEventW(EVENT_MODIFY_STATE, false, name) } {
            Ok(event) => return Ok(event),
            Err(error) => {
                last_error = Some(error);
                if attempt < ACTIVATION_SIGNAL_RETRIES {
                    thread::sleep(ACTIVATION_SIGNAL_RETRY_DELAY);
                }
            }
        }
    }

    Err(SpotlitError::platform(
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "activation event was not available".to_string()),
    ))
}
