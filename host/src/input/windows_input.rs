use anyhow::{Context, bail};
use remote_protocol::input::InputEvent;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    SendInput,
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

pub fn paste_shortcut() -> anyhow::Result<()> {
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
        keyboard(0x1d, true, false),
        keyboard(0x2f, true, false),
        keyboard(0x2f, false, false),
        keyboard(0x1d, false, false),
    ])
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
