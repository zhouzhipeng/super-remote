#[cfg(windows)]
mod windows_clipboard {
    use std::{ptr, thread, time::Duration};

    use anyhow::{Context, bail};
    use windows::Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL},
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
                OpenClipboard, SetClipboardData,
            },
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
        },
    };

    const CF_UNICODETEXT: u32 = 13;

    struct ClipboardGuard;

    impl ClipboardGuard {
        fn open() -> anyhow::Result<Self> {
            let mut last_error = None;
            for _ in 0..20 {
                match unsafe { OpenClipboard(None) } {
                    Ok(()) => return Ok(Self),
                    Err(error) => {
                        last_error = Some(error);
                        thread::sleep(Duration::from_millis(5));
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

    pub fn write_text(text: &str) -> anyhow::Result<()> {
        let utf16 = text
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let byte_len = utf16.len() * size_of::<u16>();
        let _clipboard = ClipboardGuard::open()?;
        unsafe { EmptyClipboard() }.context("failed to empty clipboard")?;
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len) }
            .context("failed to allocate clipboard memory")?;
        let pointer = unsafe { GlobalLock(memory) } as *mut u16;
        if pointer.is_null() {
            let _ = unsafe { GlobalFree(Some(memory)) };
            bail!("failed to lock clipboard memory");
        }
        unsafe { ptr::copy_nonoverlapping(utf16.as_ptr(), pointer, utf16.len()) };
        let _ = unsafe { GlobalUnlock(memory) };
        if let Err(error) = unsafe { SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))) } {
            let _ = unsafe { GlobalFree(Some(memory)) };
            return Err(error).context("failed to publish clipboard text");
        }
        // SetClipboardData transfers ownership of the allocation to Windows.
        Ok(())
    }
}

#[cfg(windows)]
pub use windows_clipboard::{read_text, write_text};

#[cfg(not(windows))]
pub fn read_text() -> anyhow::Result<String> {
    anyhow::bail!("clipboard integration is only supported on Windows")
}

#[cfg(not(windows))]
pub fn write_text(_text: &str) -> anyhow::Result<()> {
    anyhow::bail!("clipboard integration is only supported on Windows")
}
