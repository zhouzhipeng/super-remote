//! Suspends the Windows desktop effects that make the taskbar change appearance
//! with focus, for the duration of a remote session.
//!
//! Acrylic falls back to a solid colour when its surface is not focused, and an
//! active window casts a deeper shadow than an inactive one - onto the taskbar,
//! when it sits near the bottom of the screen. Both are by design and neither
//! can be held in its focused state, so the alternation is removed by removing
//! the effects. A remote session wants them gone anyway: each re-renders
//! whenever something moves behind or beneath it, and every one of those pixels
//! is damage the refinement path encodes and the video stream carries. RDP
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
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SPI_GETDROPSHADOW, SPI_SETDROPSHADOW, SPIF_SENDCHANGE,
    SPIF_UPDATEINIFILE, SendMessageTimeoutW, SystemParametersInfoW, WM_SETTINGCHANGE,
};
use windows::core::{BOOL, PCWSTR, w};

const PERSONALIZE: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
const ENABLE_TRANSPARENCY: PCWSTR = w!("EnableTransparency");
const VISUAL_EFFECTS: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VisualEffects");
const VISUAL_FX_SETTING: PCWSTR = w!("VisualFXSetting");
/// "Custom" in the Performance Options dialog. An individual effect only sticks
/// in this mode; under "let Windows choose" the per-effect bit is overridden,
/// which is why turning the shadow off on its own appeared to do nothing.
const VISUAL_FX_CUSTOM: u32 = 3;

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
/// wanted - restoring something nobody touched is how a setting gets lost. The
/// inner `None` of a registry value means it did not exist, and restoring that
/// means deleting it again rather than inventing a number the user never set.
#[derive(Default)]
struct Restore {
    transparency: Option<Option<u32>>,
    visual_fx: Option<Option<u32>>,
    drop_shadow: Option<bool>,
}

/// Restores the user's settings when the last holder drops.
pub struct DesktopEffectsGuard(());

impl DesktopEffectsGuard {
    /// Returns `None` when nothing was suspended, so a caller can hold the
    /// result unconditionally.
    pub fn acquire(transparency: bool, window_shadows: bool) -> Option<Self> {
        if !transparency && !window_shadows {
            return None;
        }
        let mut state = STATE.lock().unwrap_or_else(|error| error.into_inner());
        match state.as_mut() {
            Some(held) => held.holders += 1,
            None => {
                let restore = suspend(transparency, window_shadows);
                info!(
                    transparency = restore.transparency.is_some(),
                    visual_fx = restore.visual_fx.is_some(),
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
        // Shadow before the mode: putting the effect back while the mode still
        // says "custom" is what the user had, and flipping the mode first would
        // leave a window where neither describes their configuration.
        if let Some(previous) = restore.drop_shadow {
            set_drop_shadow(previous);
        }
        if let Some(previous) = restore.visual_fx
            && let Err(error) = write_dword(VISUAL_EFFECTS, VISUAL_FX_SETTING, previous)
        {
            warn!(%error, "could not restore the visual effects mode");
        }
        if let Some(previous) = restore.transparency {
            if let Err(error) = write_dword(PERSONALIZE, ENABLE_TRANSPARENCY, previous) {
                warn!(%error, "could not restore transparency effects");
            }
            announce();
        }
        info!("desktop effects restored");
    }
}

/// A failure on one effect is logged and skipped: a session is worth more than
/// any of them, and the others are still worth having.
fn suspend(transparency: bool, window_shadows: bool) -> Restore {
    let mut restore = Restore::default();
    if transparency {
        match read_dword(PERSONALIZE, ENABLE_TRANSPARENCY) {
            Ok(previous) if previous == Some(0) => {} // already off
            Ok(previous) => match write_dword(PERSONALIZE, ENABLE_TRANSPARENCY, Some(0)) {
                Ok(()) => {
                    restore.transparency = Some(previous);
                    announce();
                }
                Err(error) => warn!(%error, "could not suspend transparency effects"),
            },
            Err(error) => warn!(%error, "could not read the transparency setting"),
        }
    }
    if window_shadows {
        // Order matters: the mode has to allow a custom effect before setting
        // one, or Windows keeps applying its own choice.
        match read_dword(VISUAL_EFFECTS, VISUAL_FX_SETTING) {
            Ok(previous) if previous == Some(VISUAL_FX_CUSTOM) => {}
            Ok(previous) => {
                match write_dword(VISUAL_EFFECTS, VISUAL_FX_SETTING, Some(VISUAL_FX_CUSTOM)) {
                    Ok(()) => restore.visual_fx = Some(previous),
                    Err(error) => warn!(%error, "could not select the custom visual effects mode"),
                }
            }
            Err(error) => warn!(%error, "could not read the visual effects mode"),
        }
        if drop_shadow_enabled().unwrap_or(false) && set_drop_shadow(false) {
            restore.drop_shadow = Some(true);
        }
    }
    restore
}

fn drop_shadow_enabled() -> Option<bool> {
    let mut enabled = BOOL::default();
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETDROPSHADOW,
            0,
            Some((&raw mut enabled).cast()),
            Default::default(),
        )
    };
    match read {
        Ok(()) => Some(enabled.as_bool()),
        Err(error) => {
            warn!(%error, "could not read the window shadow setting");
            None
        }
    }
}

fn set_drop_shadow(enabled: bool) -> bool {
    // This action carries its boolean in pvParam itself rather than behind a
    // pointer, which is the historical Win32 convention for it. `UPDATEINIFILE`
    // is what writes the `UserPreferencesMask` bit the Performance Options
    // dialog owns; `SENDCHANGE` is what makes running shells repaint without a
    // sign-out. Sending only the latter leaves the change unrecorded.
    let value = std::ptr::without_provenance_mut(usize::from(enabled));
    match unsafe {
        SystemParametersInfoW(
            SPI_SETDROPSHADOW,
            0,
            Some(value),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        )
    } {
        Ok(()) => true,
        Err(error) => {
            warn!(%error, enabled, "could not change the window shadow setting");
            false
        }
    }
}

/// Explorer and DWM watch the personalization key, but the documented nudge is a
/// theme change broadcast; without it the repaint can wait for an unrelated
/// event. `SystemParametersInfo` broadcasts its own, so this is only for the
/// values written straight to the registry.
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

fn read_dword(path: PCWSTR, name: PCWSTR) -> anyhow::Result<Option<u32>> {
    let key = RegistryKey::open(path)?;
    let mut kind = REG_VALUE_TYPE::default();
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            name,
            None,
            Some(&mut kind),
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    status.ok().context("could not read a settings value")?;
    // Anything but a DWORD is not a setting this module understands, and
    // overwriting it would lose whatever it was.
    anyhow::ensure!(
        kind == REG_DWORD && size as usize == size_of::<u32>(),
        "unexpected settings value type"
    );
    Ok(Some(value))
}

fn write_dword(path: PCWSTR, name: PCWSTR, value: Option<u32>) -> anyhow::Result<()> {
    let key = RegistryKey::open(path)?;
    match value {
        Some(value) => unsafe {
            RegSetValueExW(key.0, name, None, REG_DWORD, Some(&value.to_le_bytes()))
        }
        .ok()
        .context("could not write a settings value"),
        None => {
            let status = unsafe { RegDeleteValueW(key.0, name) };
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(());
            }
            status.ok().context("could not clear a settings value")
        }
    }
}

struct RegistryKey(HKEY);

impl RegistryKey {
    fn open(path: PCWSTR) -> anyhow::Result<Self> {
        let mut key = HKEY::default();
        unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                path,
                None,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                &mut key,
            )
        }
        .ok()
        .context("could not open a settings key")?;
        Ok(Self(key))
    }
}

impl Drop for RegistryKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
