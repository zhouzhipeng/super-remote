//! Desktop-window privacy protection. This is not a secure-desktop/driver boundary.
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use windows::{
    Win32::{
        Foundation::HWND,
        Graphics::Dwm::{
            DWMWA_DISALLOW_PEEK, DWMWA_EXCLUDED_FROM_PEEK, DWMWA_TRANSITIONS_FORCEDISABLED,
            DwmSetWindowAttribute,
        },
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
            WindowsAndMessaging::{
                EVENT_OBJECT_REORDER, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_MENUPOPUPEND, GW_HWNDPREV, GetWindow, HWND_TOPMOST, IsWindowVisible,
                SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos, WINEVENT_OUTOFCONTEXT,
                WINEVENT_SKIPOWNPROCESS,
            },
        },
    },
    core::{BOOL, Error, Result},
};

static OVERLAY: AtomicIsize = AtomicIsize::new(0);
static REORDERING: AtomicBool = AtomicBool::new(false);

pub fn protect_from_peek(window: HWND) -> Result<()> {
    // TOPMOST alone does not prevent DWM from fading this window during another
    // window's taskbar Peek. Apply before first show; do not change global Peek.
    let enabled = BOOL(1);
    for attribute in [
        DWMWA_EXCLUDED_FROM_PEEK,
        DWMWA_DISALLOW_PEEK,
        DWMWA_TRANSITIONS_FORCEDISABLED,
    ] {
        unsafe {
            DwmSetWindowAttribute(
                window,
                attribute,
                (&enabled as *const BOOL).cast(),
                std::mem::size_of::<BOOL>() as u32,
            )?;
        }
    }
    Ok(())
}

pub fn keep_above_popups(window: HWND) {
    if !unsafe { IsWindowVisible(window) }.as_bool() || REORDERING.swap(true, Ordering::AcqRel) {
        return;
    }
    // Only repair a changed z-order: no periodic repaint, activation, or resizing.
    let mut previous = unsafe { GetWindow(window, GW_HWNDPREV) }.ok();
    while let Some(candidate) = previous.filter(|value| !value.0.is_null()) {
        if unsafe { IsWindowVisible(candidate) }.as_bool() {
            unsafe {
                let _ = SetWindowPos(
                    window,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                );
            }
            break;
        }
        previous = unsafe { GetWindow(candidate, GW_HWNDPREV) }.ok();
    }
    REORDERING.store(false, Ordering::Release);
}

pub struct PrivacyHooks(Vec<HWINEVENTHOOK>);

impl PrivacyHooks {
    pub fn install(window: HWND) -> Result<Self> {
        let mut hooks = Self(Vec::new());
        for (first, last) in [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MENUPOPUPEND),
            (EVENT_OBJECT_SHOW, EVENT_OBJECT_REORDER),
        ] {
            let hook = unsafe {
                SetWinEventHook(
                    first,
                    last,
                    None,
                    Some(on_window_event),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if hook.0.is_null() {
                return Err(Error::from_thread());
            }
            hooks.0.push(hook);
        }
        OVERLAY.store(window.0 as isize, Ordering::Release);
        Ok(hooks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, SW_SHOWNA, ShowWindow, WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
        core::w,
    };

    struct TestWindow(HWND);
    impl TestWindow {
        fn new() -> Self {
            // Isolated off-screen windows: never touch the user's privacy settings
            // or move/activate any application on their desktop.
            Self(unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    w!("STATIC"),
                    w!("Super Remote privacy regression"),
                    WS_POPUP,
                    -30000,
                    -30000,
                    16,
                    16,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap()
            })
        }
    }
    impl Drop for TestWindow {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyWindow(self.0);
            }
        }
    }

    #[test]
    fn peek_protection_and_popup_order_preserve_hidden_state_and_focus() {
        let overlay = TestWindow::new();
        protect_from_peek(overlay.0).unwrap();
        // These DWM attributes are set-only; successful setters are the supported
        // API check, not DwmGetWindowAttribute (which returns E_INVALIDARG).
        let hooks = PrivacyHooks::install(overlay.0).unwrap();
        keep_above_popups(overlay.0);
        assert!(!unsafe { IsWindowVisible(overlay.0) }.as_bool());
        unsafe {
            let _ = ShowWindow(overlay.0, SW_SHOWNA);
        }
        let popup = TestWindow::new();
        unsafe {
            let _ = ShowWindow(popup.0, SW_SHOWNA);
        }
        let foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
        keep_above_popups(overlay.0);
        // The installed privacy panel may itself raise a third window during
        // this test. Assert relative order, not adjacency in the global stack.
        let mut above = unsafe { GetWindow(popup.0, GW_HWNDPREV) }.ok();
        let mut found = false;
        for _ in 0..1024 {
            let Some(window) = above.filter(|window| !window.0.is_null()) else { break; };
            if window == overlay.0 { found = true; break; }
            above = unsafe { GetWindow(window, GW_HWNDPREV) }.ok();
        }
        assert!(found, "privacy overlay must be above the test popup");
        assert_eq!(foreground, unsafe {
            windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()
        });
        drop(hooks);
        assert_eq!(OVERLAY.load(Ordering::Acquire), 0);
    }
}

impl Drop for PrivacyHooks {
    fn drop(&mut self) {
        OVERLAY.store(0, Ordering::Release);
        for hook in self.0.drain(..) {
            unsafe {
                let _ = UnhookWinEvent(hook);
            }
        }
    }
}

unsafe extern "system" fn on_window_event(
    _: HWINEVENTHOOK,
    _: u32,
    _: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    // OUTOFCONTEXT callbacks run on the panel's message thread. No App borrow or
    // input injection; native menu/thumbnail creation repairs the layer promptly.
    let window = OVERLAY.load(Ordering::Acquire);
    if window != 0 {
        keep_above_popups(HWND(window as *mut _));
    }
}
