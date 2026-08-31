use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::Context;
use tracing::{info, warn};
use windows::Win32::{
    Foundation::{LPARAM, WPARAM},
    System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    },
    UI::WindowsAndMessaging::{
        HWND_BROADCAST, SC_MONITORPOWER, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SYSCOMMAND,
    },
};

/// Keeps the interactive display awake for exactly one active video session.
///
/// Desktop Duplication cannot produce a usable frame while Windows has powered
/// down the display output. SetThreadExecutionState is thread-scoped, so the
/// request lives on a dedicated thread and is explicitly cleared on drop.
pub struct DisplayPowerGuard {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl DisplayPowerGuard {
    pub fn acquire() -> anyhow::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("display-power-request".into())
            .spawn(move || {
                let previous = unsafe {
                    SetThreadExecutionState(
                        ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
                    )
                };
                if previous.0 == 0 {
                    let _ = ready_tx.send(Err(io::Error::last_os_error()));
                    return;
                }

                // ES_DISPLAY_REQUIRED resets the display idle timer. The
                // broadcast additionally wakes an output that is already off.
                let mut message_result = 0usize;
                let wake_result = unsafe {
                    SendMessageTimeoutW(
                        HWND_BROADCAST,
                        WM_SYSCOMMAND,
                        WPARAM(SC_MONITORPOWER as usize),
                        LPARAM(-1),
                        SMTO_ABORTIFHUNG,
                        500,
                        Some(&mut message_result),
                    )
                };
                if wake_result.0 == 0 {
                    warn!(
                        error = %io::Error::last_os_error(),
                        "display wake broadcast was not acknowledged"
                    );
                }

                // Let the display driver recreate its scanout before DXGI
                // Desktop Duplication opens. Starting capture earlier produces
                // an initial black frame on several NVIDIA drivers.
                thread::sleep(Duration::from_millis(750));
                let _ = ready_tx.send(Ok(()));
                info!("display is awake and held on for the active client");

                while !worker_stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(100));
                }
                unsafe {
                    SetThreadExecutionState(ES_CONTINUOUS);
                }
                info!("display power request released");
            })
            .context("failed to start the display power request thread")?;

        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error).context("Windows rejected the display power request")
            }
            Err(error) => {
                stop.store(true, Ordering::Release);
                let _ = worker.join();
                Err(error).context("timed out while waking the display")
            }
        }
    }
}

impl Drop for DisplayPowerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
