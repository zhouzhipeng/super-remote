//! Suspends Windows desktop effects for the duration of a remote session.
//!
//! Every one of these effects renders differently depending on which window has
//! focus, and the capture path has no idea focus exists - so a difference that
//! follows focus is always the desktop itself, faithfully transmitted. Acrylic
//! falls back to a solid colour when its surface is not focused, and an active
//! window casts a deeper shadow than an inactive one, so over a remote view the
//! taskbar appears to gain and lose a shadow as focus moves. There is no
//! supported way to hold either effect in its focused state.
//!
//! A remote session wants them off anyway. Each effect re-renders whenever
//! something moves behind or beneath it, and every one of those pixels is damage
//! the refinement path has to encode and the video stream has to carry. RDP
//! disables desktop effects for the same reason.
//!
//! Everything here is restored when the last session releases the guard.

use std::sync::Mutex;

use anyhow::Context;
use tracing::{info, warn};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_VALUE_TYPE,
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SPI_GETDROPSHADOW, SPI_GETUIEFFECTS, SPI_SETDROPSHADOW,
    SPI_SETUIEFFECTS, SPIF_SENDCHANGE, SYSTEM_PARAMETERS_INFO_ACTION,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageTimeoutW, SystemParametersInfoW,
    WM_SETTINGCHANGE,
};
use windows::core::{BOOL, PCWSTR, w};

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
const ENABLE_TRANSPARENCY: PCWSTR = w!("EnableTransparency");

/// Shared because a session restarts its video pipeline on display changes and
/// refinement holds a guard of its own. Only the first holder may read what the
/// user had configured; a later one would read the values this module wrote and
/// restore *those* on release, turning a temporary change into a permanent one.
static STATE: Mutex<Option<Held>> = Mutex::new(None);

struct Held {
    restore: Restore,
    holders: usize,
}

/// What to put back. A field is `None` when this module did not change it,
/// either because it was not asked to or because the value was already what it
/// wanted - restoring something nobody touched is how a setting gets lost.
#[derive(Default)]
struct Restore {
    /// The inner `None` means the value did not exist, which Windows reads as
    /// enabled; restoring that means deleting it again, not writing a 1.
    transparency: Option<Option<u32>>,
    ui_effects: Option<bool>,
    drop_shadow: Option<bool>,
}

/// Restores the user's settings when the last holder drops.
pub struct DesktopEffectsGuard(());

impl DesktopEffectsGuard {
    /// Returns `None` when nothing was suspended, so a caller can hold the
    /// result unconditionally.
    pub fn acquire(transparency: bool, visual_effects: bool) -> Option<Self> {
        if !transparency && !visual_effects {
            return None;
        }
        let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        match state.as_mut() {
            Some(held) => held.holders += 1,
            None => {
                let restore = suspend(transparency, visual_effects);
                info!(
                    transparency = restore.transparency.is_some(),
                    ui_effects = restore.ui_effects.is_some(),
                    drop_shadow = restore.drop_shadow.is_some(),
                    "desktop effects suspended for this session"
                );
                *state = Some(Held {
                    restore,
                    holders: 1,
                });
            }
        }
        Some(Self(()))
    }
}

impl Drop for DesktopEffectsGuard {
    fn drop(&mut self) {
        let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        let Some(held) = state.as_mut() else { return };
        held.holders -= 1;
        if held.holders > 0 {
            return;
        }
        let restore = std::mem::take(&mut held.restore);
        *state = None;
        if let Some(previous) = restore.transparency
            && let Err(error) = restore_transparency(previous)
        {
            warn!(%error, "could not restore transparency effects");
        }
        if let Some(previous) = restore.ui_effects {
            set_flag(SPI_SETUIEFFECTS, previous, "UI effects");
        }
        if let Some(previous) = restore.drop_shadow {
            set_flag(SPI_SETDROPSHADOW, previous, "window shadows");
        }
        info!("desktop effects restored");
    }
}

/// A failure to suspend one effect is logged and skipped: a session is worth
/// more than any of them, and the others are still worth having.
fn suspend(transparency: bool, visual_effects: bool) -> Restore {
    let mut restore = Restore::default();
    if transparency {
        match suspend_transparency() {
            Ok(previous) => restore.transparency = previous,
            Err(error) => warn!(%error, "could not suspend transparency effects"),
        }
    }
    if visual_effects {
        // The master switch behind "adjust for best performance": menu and
        // tooltip animation, fades, gradient captions, cursor and window
        // shadows. `SystemParametersInfo` applies it live and writes the same
        // preferences the Performance Options dialog does, where editing
        // `UserPreferencesMask` directly would need a sign-out to take effect.
        restore.ui_effects = suspend_flag(SPI_GETUIEFFECTS, SPI_SETUIEFFECTS, "UI effects");
        // Redundant with the master switch on paper, but it is the setting that
        // actually names window shadows, and the two have drifted apart across
        // Windows releases.
        restore.drop_shadow = suspend_flag(SPI_GETDROPSHADOW, SPI_SETDROPSHADOW, "window shadows");
    }
    restore
}

/// Returns the previous value only when it was changed, so release puts back
/// exactly what was there and nothing else.
fn suspend_flag(
    get: SYSTEM_PARAMETERS_INFO_ACTION,
    set: SYSTEM_PARAMETERS_INFO_ACTION,
    what: &str,
) -> Option<bool> {
    let mut enabled = BOOL::default();
    let read = unsafe {
        SystemParametersInfoW(
            get,
            0,
            Some((&raw mut enabled).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if let Err(error) = read {
        warn!(%error, %what, "could not read a desktop effect");
        return None;
    }
    if !enabled.as_bool() {
        return None;
    }
    set_flag(set, false, what).then_some(true)
}

fn set_flag(action: SYSTEM_PARAMETERS_INFO_ACTION, enabled: bool, what: &str) -> bool {
    // These actions carry their boolean in pvParam itself rather than behind a
    // pointer, which is the historical Win32 convention for them.
    let value = std::ptr::without_provenance_mut(usize::from(enabled));
    match unsafe { SystemParametersInfoW(action, 0, Some(value), SPIF_SENDCHANGE) } {
        Ok(()) => true,
        Err(error) => {
            warn!(%error, %what, enabled, "could not change a desktop effect");
            false
        }
    }
}

/// Writes 0 and returns what was there, or `None` when nothing was changed.
fn suspend_transparency() -> anyhow::Result<Option<Option<u32>>> {
    let key = PersonalizeKey::open()?;
    let previous = key.read()?;
    if previous == Some(0) {
        // Already off: nothing to change, and nothing to restore either.
        return Ok(None);
    }
    key.write(0)?;
    announce();
    Ok(Some(previous))
}

fn restore_transparency(previous: Option<u32>) -> anyhow::Result<()> {
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
