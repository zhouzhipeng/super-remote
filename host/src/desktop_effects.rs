//! Suspends Windows transparency effects for the duration of a remote session.
//!
//! Acrylic and Mica fall back to a solid colour whenever their surface is not
//! focused. That is by design, so the taskbar genuinely alternates between two
//! appearances as focus moves, and over a remote view the darker one reads as a
//! shadow that should not be there. There is no supported way to hold acrylic in
//! its focused state - `AlwaysUseFallback` only forces the opposite - so the
//! only way to stop the alternation is to turn the effect off, which is what a
//! remote session wants anyway: a blurred backdrop re-renders whenever anything
//! moves behind it, and every one of those pixels has to be encoded and sent.
//!
//! The original setting is restored when the last session releases the guard.

use std::sync::Mutex;

use anyhow::Context;
use tracing::{info, warn};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_VALUE_TYPE,
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use windows::core::{PCWSTR, w};

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
const ENABLE_TRANSPARENCY: PCWSTR = w!("EnableTransparency");

/// Shared because a session restarts its video pipeline on display changes and
/// refinement holds a guard of its own. Only the first holder may read what the
/// user had configured; a later one would read the value this module wrote and
/// restore *that* on release, turning a temporary change into a permanent one.
static STATE: Mutex<Option<Held>> = Mutex::new(None);

struct Held {
    /// `None` when the value did not exist, which Windows reads as enabled.
    previous: Option<u32>,
    holders: usize,
}

/// Restores the user's setting when the last holder drops.
pub struct TransparencyGuard(());

impl TransparencyGuard {
    /// Returns `None` when suspension is disabled by configuration, so a caller
    /// can hold the result unconditionally.
    pub fn acquire(enabled: bool) -> Option<Self> {
        if !enabled {
            return None;
        }
        let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        match state.as_mut() {
            Some(held) => held.holders += 1,
            None => match suspend() {
                Ok(previous) => {
                    info!(?previous, "transparency effects suspended for this session");
                    *state = Some(Held {
                        previous,
                        holders: 1,
                    });
                }
                // A session is worth more than the effect, so this is not fatal.
                Err(error) => {
                    warn!(%error, "could not suspend transparency effects");
                    return None;
                }
            },
        }
        Some(Self(()))
    }
}

impl Drop for TransparencyGuard {
    fn drop(&mut self) {
        let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let Some(held) = state.as_mut() else { return };
        held.holders -= 1;
        if held.holders > 0 {
            return;
        }
        let previous = held.previous;
        *state = None;
        match restore(previous) {
            Ok(()) => info!("transparency effects restored"),
            Err(error) => warn!(%error, "could not restore transparency effects"),
        }
    }
}

/// Writes 0 and returns what was there, so the caller can put it back.
fn suspend() -> anyhow::Result<Option<u32>> {
    let key = PersonalizeKey::open()?;
    let previous = key.read()?;
    if previous == Some(0) {
        // Already off: nothing to change, and nothing to restore either.
        return Ok(previous);
    }
    key.write(0)?;
    announce();
    Ok(previous)
}

fn restore(previous: Option<u32>) -> anyhow::Result<()> {
    if previous == Some(0) {
        return Ok(());
    }
    let key = PersonalizeKey::open()?;
    match previous {
        Some(value) => key.write(value)?,
        // Absent means enabled. Deleting is what restores that exactly, rather
        // than leaving behind a value the user never set.
        None => key.delete()?,
    }
    announce();
    Ok(())
}

/// Explorer and DWM watch the key, but the documented nudge is a theme change
/// broadcast; without it the repaint can wait for an unrelated event.
fn announce() {
    let mut ignored = 0usize;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            500,
            Some(&mut ignored),
        );
    }
}

struct PersonalizeKey(HKEY);

impl PersonalizeKey {
    fn open() -> anyhow::Result<Self> {
        let mut key = HKEY::default();
        unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PERSONALIZE,
                None,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                &mut key,
            )
        }
        .ok()
        .context("could not open the personalization key")?;
        Ok(Self(key))
    }

    fn read(&self) -> anyhow::Result<Option<u32>> {
        let mut kind = REG_VALUE_TYPE::default();
        let mut value = 0u32;
        let mut size = size_of::<u32>() as u32;
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                ENABLE_TRANSPARENCY,
                None,
                Some(&mut kind),
                Some((&raw mut value).cast()),
                Some(&mut size),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok().context("could not read the transparency setting")?;
        // Anything but a DWORD is not a setting this module understands, and
        // overwriting it would lose whatever it was.
        anyhow::ensure!(
            kind == REG_DWORD && size as usize == size_of::<u32>(),
            "unexpected transparency setting type"
        );
        Ok(Some(value))
    }

    fn write(&self, value: u32) -> anyhow::Result<()> {
        unsafe {
            RegSetValueExW(
                self.0,
                ENABLE_TRANSPARENCY,
                None,
                REG_DWORD,
                Some(&value.to_le_bytes()),
            )
        }
        .ok()
        .context("could not write the transparency setting")
    }

    fn delete(&self) -> anyhow::Result<()> {
        let status = unsafe { RegDeleteValueW(self.0, ENABLE_TRANSPARENCY) };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        status
            .ok()
            .context("could not clear the transparency setting")
    }
}

impl Drop for PersonalizeKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
