use anyhow::{Context, bail};
use remote_protocol::input::InputEvent;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
};

pub fn inject(event: InputEvent) -> anyhow::Result<()> {
    let input = match event {
        InputEvent::MouseMove { x, y } => mouse(
            x as i32,
            y as i32,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
        ),
        InputEvent::MouseRelative { dx, dy } => mouse(dx as i32, dy as i32, 0, MOUSEEVENTF_MOVE),
        InputEvent::MouseWheel { delta_x, delta_y } => {
            let mut inputs = Vec::with_capacity(2);
            if delta_x != 0 {
                inputs.push(mouse(0, 0, delta_x as i32 as u32, MOUSEEVENTF_HWHEEL));
            }
            if delta_y != 0 {
                inputs.push(mouse(0, 0, delta_y as i32 as u32, MOUSEEVENTF_WHEEL));
            }
            return send(&inputs);
        }
        InputEvent::MouseButton {
            button,
            down,
            position,
        } => {
            let flags = match (button, down) {
                (0, true) => MOUSEEVENTF_LEFTDOWN,
                (0, false) => MOUSEEVENTF_LEFTUP,
                (1, true) => MOUSEEVENTF_MIDDLEDOWN,
                (1, false) => MOUSEEVENTF_MIDDLEUP,
                (2, true) => MOUSEEVENTF_RIGHTDOWN,
                (2, false) => MOUSEEVENTF_RIGHTUP,
                _ => bail!("unsupported mouse button {button}"),
            };
            let button_input = mouse(0, 0, 0, flags);
            if let Some((x, y)) = position {
                // Move and transition are one SendInput batch, so a click can
                // never wait behind or overtake an unreliable position packet.
                return send(&[
                    mouse(
                        x as i32,
                        y as i32,
                        0,
                        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                    ),
                    button_input,
                ]);
            }
            button_input
        }
        InputEvent::Keyboard {
            scan_code,
            down,
            extended,
        } => keyboard(scan_code, down, extended),
    };
    send(&[input])
}

pub fn paste_text(text: &str) -> anyhow::Result<()> {
    // A browser shortcut can leave Cmd-as-Ctrl or another modifier logically
    // held until its later keyup arrives over the data channel. Clear every
    // modifier first so the target always receives exactly Ctrl+V.
    send(&[
        keyboard(0x1d, false, false), // left Ctrl
        keyboard(0x1d, false, true),  // right Ctrl
        keyboard(0x2a, false, false), // left Shift
        keyboard(0x36, false, false), // right Shift
        keyboard(0x38, false, false), // left Alt
        keyboard(0x38, false, true),  // right Alt
        keyboard(0x5b, false, true),  // left Windows
        keyboard(0x5c, false, true),  // right Windows
    ])?;
    // Inject the captured browser text itself. This is keyboard-layout
    // independent and does not depend on the foreground application accepting
    // a synthetic Ctrl+V shortcut. UTF-16 code units preserve all Unicode,
    // including surrogate pairs.
    let inputs = text
        .encode_utf16()
        .flat_map(|unit| [unicode(unit, true), unicode(unit, false)])
        .collect::<Vec<_>>();
    for chunk in inputs.chunks(512) {
        send(chunk)?;
    }
    Ok(())
}

fn unicode(unit: u16, down: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wScan: unit,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn keyboard(scan_code: u16, down: bool, extended: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wScan: scan_code,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn mouse(
    dx: i32,
    dy: i32,
    mouse_data: u32,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn send(inputs: &[INPUT]) -> anyhow::Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        Err(std::io::Error::last_os_error()).context("SendInput rejected an input event")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::paste_text;
    use windows::{
        Win32::UI::{
            Input::KeyboardAndMouse::SetFocus,
            WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, GetWindowTextW, MSG, PM_REMOVE,
                PeekMessageW, SW_SHOW, SetForegroundWindow, ShowWindow, TranslateMessage,
                WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
            },
        },
        core::w,
    };

    #[test]
    #[ignore = "opens a short-lived native edit window to verify real SendInput delivery"]
    fn unicode_paste_reaches_the_focused_windows_control() {
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                w!(""),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                100,
                100,
                500,
                180,
                None,
                None,
                None,
                None,
            )
        }
        .expect("create native edit control");
        unsafe {
            let _ = ShowWindow(window, SW_SHOW);
            let _ = SetForegroundWindow(window);
            let _ = SetFocus(Some(window));
        }

        let expected = "Super Remote 双向粘贴 ✅";
        paste_text(expected).expect("inject Unicode text");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut actual = String::new();
        while std::time::Instant::now() < deadline {
            let mut message = MSG::default();
            while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            let mut buffer = vec![0_u16; 256];
            let copied = unsafe { GetWindowTextW(window, &mut buffer) };
            actual = String::from_utf16_lossy(&buffer[..copied.max(0) as usize]);
            if actual == expected {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        unsafe { DestroyWindow(window).expect("destroy native edit control") };
        assert_eq!(actual, expected);
    }
}
