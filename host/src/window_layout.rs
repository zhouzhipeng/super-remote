use anyhow::{Context, bail};
use tracing::warn;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, POINT, RECT},
        Graphics::{
            Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
            Gdi::{
                GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
                MonitorFromPoint, MonitorFromWindow,
            },
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GetClassNameW, GetShellWindow, GetSystemMetrics, GetWindowPlacement,
            IsIconic, IsWindowVisible, IsZoomed, SM_CMONITORS, SW_MAXIMIZE, SW_MINIMIZE,
            SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow, WINDOWPLACEMENT,
        },
    },
    core::BOOL,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MoveSummary {
    pub monitor_count: i32,
    pub candidates: usize,
    pub moved: usize,
    pub failed: usize,
}

struct MoveContext {
    primary: windows::Win32::Graphics::Gdi::HMONITOR,
    primary_work: RECT,
    candidates: usize,
    moved: usize,
    failed: usize,
}

pub fn move_secondary_windows_to_primary() -> anyhow::Result<MoveSummary> {
    let monitor_count = unsafe { GetSystemMetrics(SM_CMONITORS) };
    if monitor_count <= 1 {
        return Ok(MoveSummary {
            monitor_count,
            ..Default::default()
        });
    }

    let primary = unsafe { MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY) };
    let primary_work = monitor_work_area(primary)?;
    let mut context = MoveContext {
        primary,
        primary_work,
        candidates: 0,
        moved: 0,
        failed: 0,
    };
    unsafe {
        EnumWindows(
            Some(move_window_callback),
            LPARAM((&mut context as *mut MoveContext) as isize),
        )
    }
    .context("EnumWindows failed")?;
    Ok(MoveSummary {
        monitor_count,
        candidates: context.candidates,
        moved: context.moved,
        failed: context.failed,
    })
}

unsafe extern "system" fn move_window_callback(window: HWND, parameter: LPARAM) -> BOOL {
    let context = unsafe { &mut *(parameter.0 as *mut MoveContext) };
    if !is_application_window(window) {
        return true.into();
    }
    let source_monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
    if source_monitor == context.primary {
        return true.into();
    }
    context.candidates += 1;

    let Ok(source_work) = monitor_work_area(source_monitor) else {
        context.failed += 1;
        return true.into();
    };
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    if unsafe { GetWindowPlacement(window, &mut placement) }.is_err() {
        context.failed += 1;
        return true.into();
    }
    let destination = map_window_rect(
        placement.rcNormalPosition,
        source_work,
        context.primary_work,
    );
    let was_minimized = unsafe { IsIconic(window) }.as_bool();
    let was_maximized = unsafe { IsZoomed(window) }.as_bool();
    if was_minimized || was_maximized {
        // WINDOWPLACEMENT restore coordinates are relative to the source monitor's
        // workspace, so changing them alone cannot select another monitor. Restore,
        // perform one real screen-coordinate move, then reinstate the original state.
        let _ = unsafe { ShowWindow(window, SW_RESTORE) };
    }
    let result = unsafe {
        SetWindowPos(
            window,
            None,
            destination.left,
            destination.top,
            destination.right - destination.left,
            destination.bottom - destination.top,
            SWP_NOACTIVATE | SWP_NOZORDER,
        )
    };
    if result.is_ok() && was_minimized {
        let _ = unsafe { ShowWindow(window, SW_MINIMIZE) };
    } else if result.is_ok() && was_maximized {
        let _ = unsafe { ShowWindow(window, SW_MAXIMIZE) };
    }
    let reached_primary =
        unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) } == context.primary;
    if result.is_ok() && reached_primary {
        context.moved += 1;
    } else {
        context.failed += 1;
        warn!(
            class = %window_class(window),
            source_left = source_work.left,
            source_top = source_work.top,
            source_right = source_work.right,
            source_bottom = source_work.bottom,
            normal_left = placement.rcNormalPosition.left,
            normal_top = placement.rcNormalPosition.top,
            normal_right = placement.rcNormalPosition.right,
            normal_bottom = placement.rcNormalPosition.bottom,
            destination_left = destination.left,
            destination_top = destination.top,
            destination_right = destination.right,
            destination_bottom = destination.bottom,
            api_error = ?result.err(),
            reached_primary,
            "application window did not move to the primary monitor"
        );
    }
    true.into()
}

fn is_application_window(window: HWND) -> bool {
    if !unsafe { IsWindowVisible(window) }.as_bool() || window == unsafe { GetShellWindow() } {
        return false;
    }
    let mut cloaked = 0u32;
    if unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    }
    .is_ok()
        && cloaked != 0
    {
        return false;
    }
    !matches!(
        window_class(window).as_str(),
        "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "SuperRemotePrivacyOverlay"
            | "#32768"
            | "tooltips_class32"
            | "SysShadow"
    )
}

fn window_class(window: HWND) -> String {
    let mut buffer = [0u16; 256];
    let length = unsafe { GetClassNameW(window, &mut buffer) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

fn monitor_work_area(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> anyhow::Result<RECT> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        bail!("GetMonitorInfoW failed")
    }
    Ok(info.rcWork)
}

fn map_window_rect(window: RECT, source: RECT, destination: RECT) -> RECT {
    let width = (window.right - window.left).max(1);
    let height = (window.bottom - window.top).max(1);
    let (left, width) = map_axis(
        window.left,
        width,
        source.left,
        source.right,
        destination.left,
        destination.right,
    );
    let (top, height) = map_axis(
        window.top,
        height,
        source.top,
        source.bottom,
        destination.top,
        destination.bottom,
    );
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

fn map_axis(
    position: i32,
    size: i32,
    source_start: i32,
    source_end: i32,
    destination_start: i32,
    destination_end: i32,
) -> (i32, i32) {
    let source_size = (source_end - source_start).max(1);
    let destination_size = (destination_end - destination_start).max(1);
    let mapped_size = size.clamp(1, destination_size);
    let source_travel = (source_size - size).max(0);
    let destination_travel = destination_size - mapped_size;
    let source_offset = (position - source_start).clamp(0, source_travel);
    let destination_offset = if source_travel == 0 {
        0
    } else {
        ((source_offset as i64 * destination_travel as i64) / source_travel as i64) as i32
    };
    (destination_start + destination_offset, mapped_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_size_and_relative_position() {
        let mapped = map_window_rect(
            RECT {
                left: 2560,
                top: 400,
                right: 3560,
                bottom: 1200,
            },
            RECT {
                left: 2560,
                top: 0,
                right: 5120,
                bottom: 1400,
            },
            RECT {
                left: 0,
                top: 0,
                right: 2560,
                bottom: 1560,
            },
        );
        assert_eq!(mapped.right - mapped.left, 1000);
        assert_eq!(mapped.bottom - mapped.top, 800);
        assert!(mapped.left >= 0 && mapped.right <= 2560);
        assert!(mapped.top >= 0 && mapped.bottom <= 1560);
    }

    #[test]
    fn clamps_oversized_window_to_primary_work_area() {
        let destination = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(
            map_window_rect(
                RECT {
                    left: -3840,
                    top: 0,
                    right: -1280,
                    bottom: 1440,
                },
                RECT {
                    left: -3840,
                    top: 0,
                    right: -1280,
                    bottom: 1400,
                },
                destination,
            ),
            destination
        );
    }
}
