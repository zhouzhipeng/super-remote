#[cfg(windows)]
mod windows_clipboard {
    use std::{thread, time::Duration};

    use anyhow::{Context, bail};
    use windows::Win32::{
        Foundation::HGLOBAL,
        System::{
            DataExchange::{
                CloseClipboard, GetClipboardData, GetClipboardSequenceNumber,
                IsClipboardFormatAvailable, OpenClipboard,
            },
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
        },
    };

    const CF_UNICODETEXT: u32 = 13;

    struct ClipboardGuard;

    impl ClipboardGuard {
        fn open() -> anyhow::Result<Self> {
            let mut last_error = None;
            for _ in 0..30 {
                match unsafe { OpenClipboard(None) } {
                    Ok(()) => return Ok(Self),
                    Err(error) => {
                        last_error = Some(error);
                        thread::sleep(Duration::from_millis(3));
                    }
                }
            }
            Err(last_error.expect("OpenClipboard was attempted")).context("clipboard is busy")
        }
    }

    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            let _ = unsafe { CloseClipboard() };
        }
    }

    pub fn read_text() -> anyhow::Result<String> {
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_err() {
            return Ok(String::new());
        }
        let _clipboard = ClipboardGuard::open()?;
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) }
            .context("clipboard does not contain Unicode text")?;
        let memory = HGLOBAL(handle.0);
        let byte_len = unsafe { GlobalSize(memory) };
        if byte_len < 2 {
            return Ok(String::new());
        }
        let pointer = unsafe { GlobalLock(memory) } as *const u16;
        if pointer.is_null() {
            bail!("failed to lock clipboard memory");
        }
        let units = unsafe { std::slice::from_raw_parts(pointer, byte_len / 2) };
        let text_len = units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len());
        let result = String::from_utf16(&units[..text_len]);
        let _ = unsafe { GlobalUnlock(memory) };
        result.context("clipboard text is not UTF-16")
    }

    pub fn read_text_after_copy(timeout: Duration) -> anyhow::Result<String> {
        let initial_sequence = unsafe { GetClipboardSequenceNumber() };
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if unsafe { GetClipboardSequenceNumber() } != initial_sequence {
                // Some applications update multiple clipboard formats in quick
                // succession. Wait one scheduler slice before opening it.
                thread::sleep(Duration::from_millis(3));
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        read_text()
    }
}

#[cfg(windows)]
pub use windows_clipboard::read_text_after_copy;

#[cfg(not(windows))]
pub fn read_text_after_copy(_timeout: std::time::Duration) -> anyhow::Result<String> {
    anyhow::bail!("clipboard integration is only supported on Windows")
}
