use base64::Engine;
use serde::Serialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use windows::Win32::{Graphics::Gdi::*, UI::WindowsAndMessaging::*};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_cursor_can_be_encoded_without_moving_the_pointer() {
        unsafe {
            let cursor = LoadCursorW(None, IDC_ARROW).unwrap();
            assert_eq!(standard_shape(cursor), Some("default"));
            // The active desktop may replace IDC_ARROW with a transparent
            // cursor (remote sessions/accessibility). Test pixels with an
            // owned deterministic cursor without changing the system cursor.
            let mut mask = [255u8; 128];
            let mut color = [0u8; 128];
            for row in 8..24 { mask[row * 4 + 1] = 0; color[row * 4 + 1] = 255; }
            let fixture = CreateCursor(None, 0, 0, 32, 32, mask.as_ptr().cast(), color.as_ptr().cast()).unwrap();
            let encoded = cursor_image(fixture);
            DestroyCursor(fixture).unwrap();
            let image = encoded.unwrap();
            let png = base64::engine::general_purpose::STANDARD
                .decode(image.png)
                .unwrap();
            let decoder = png::Decoder::new(std::io::Cursor::new(png));
            let mut reader = decoder.read_info().unwrap();
            let mut pixels = vec![0; reader.output_buffer_size()];
            let frame = reader.next_frame(&mut pixels).unwrap();
            assert!(image.x < frame.width && image.y < frame.height);
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] != 0));
            assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 0));
        }
    }
}

#[derive(Serialize)]
struct CursorState {
    visible: bool,
    shape: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<CursorImage>,
}

#[derive(Serialize)]
struct CursorImage {
    png: String,
    x: u32,
    y: u32,
}

pub async fn serve(channel: Arc<dyn DataChannel>, retired: Arc<AtomicBool>) {
    let mut interval = tokio::time::interval(Duration::from_millis(32));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut previous = String::new();
    let mut cached_handle = 0usize;
    let mut cached_shape = ("default", None);
    loop {
        tokio::select! {
            event = channel.poll() => {
                if matches!(event, None | Some(DataChannelEvent::OnClose)) { break; }
            }
            _ = interval.tick() => {
                if retired.load(Ordering::Acquire) { break; }
                let snapshot = unsafe {
                    let mut info = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
                    if GetCursorInfo(&mut info).is_err() { continue; }
                    let handle = info.hCursor.0 as usize;
                    if handle != cached_handle {
                        cached_handle = handle;
                        let shape = standard_shape(info.hCursor);
                        cached_shape = (shape.unwrap_or("default"), if shape.is_none() { cursor_image(info.hCursor).ok() } else { None });
                    }
                    CursorState {
                        visible: info.flags == CURSOR_SHOWING,
                        shape: cached_shape.0,
                        image: cached_shape.1.as_ref().map(|v| CursorImage { png: v.png.clone(), x: v.x, y: v.y }),
                    }
                };
                let Ok(message) = serde_json::to_string(&snapshot) else { continue; };
                if message != previous && channel.outstanding_bytes().await.unwrap_or(1) == 0
                    && channel.try_send_text(&message).await.is_ok() { previous = message; }
            }
        }
    }
}

unsafe fn standard_shape(cursor: HCURSOR) -> Option<&'static str> {
    for (id, name) in [
        (IDC_ARROW, "default"),
        (IDC_IBEAM, "text"),
        (IDC_HAND, "pointer"),
        (IDC_WAIT, "wait"),
        (IDC_APPSTARTING, "progress"),
        (IDC_CROSS, "crosshair"),
        (IDC_SIZEALL, "move"),
        (IDC_SIZENS, "ns-resize"),
        (IDC_SIZEWE, "ew-resize"),
        (IDC_SIZENESW, "nesw-resize"),
        (IDC_SIZENWSE, "nwse-resize"),
        (IDC_NO, "not-allowed"),
        (IDC_HELP, "help"),
        (IDC_UPARROW, "default"),
    ] {
        if unsafe { LoadCursorW(None, id) }.ok() == Some(cursor) {
            return Some(name);
        }
    }
    None
}

// Owned GDI resources are released on every error path.
struct IconBitmaps(ICONINFO);
impl Drop for IconBitmaps {
    fn drop(&mut self) {
        unsafe {
            if !self.0.hbmColor.is_invalid() {
                let _ = DeleteObject(self.0.hbmColor.into());
            }
            if !self.0.hbmMask.is_invalid() {
                let _ = DeleteObject(self.0.hbmMask.into());
            }
        }
    }
}
struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            if !self.old.is_invalid() {
                SelectObject(self.dc, self.old);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            let _ = DeleteDC(self.dc);
        }
    }
}

unsafe fn cursor_image(cursor: HCURSOR) -> anyhow::Result<CursorImage> {
    unsafe {
        let mut info = IconBitmaps(ICONINFO::default());
        GetIconInfo(HICON(cursor.0), &mut info.0)?;
        let mut bitmap = BITMAP::default();
        let monochrome = info.0.hbmColor.is_invalid();
        let source = if monochrome {
            info.0.hbmMask
        } else {
            info.0.hbmColor
        };
        anyhow::ensure!(
            GetObjectW(
                source.into(),
                std::mem::size_of::<BITMAP>() as i32,
                Some((&mut bitmap as *mut BITMAP).cast())
            ) != 0,
            "cursor bitmap unavailable"
        );
        let width = bitmap.bmWidth;
        let height = bitmap.bmHeight / if monochrome { 2 } else { 1 };
        anyhow::ensure!(
            (1..=128).contains(&width) && (1..=128).contains(&height),
            "cursor exceeds browser limit"
        );
        let dc = CreateCompatibleDC(None);
        anyhow::ensure!(!dc.is_invalid(), "cursor DC unavailable");
        let mut surface = Surface {
            dc,
            bitmap: HBITMAP::default(),
            old: HGDIOBJ::default(),
        };
        let header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        surface.bitmap = CreateDIBSection(Some(dc), &header, DIB_RGB_COLORS, &mut bits, None, 0)?;
        surface.old = SelectObject(dc, surface.bitmap.into());
        let pixels =
            std::slice::from_raw_parts_mut(bits.cast::<u8>(), (width * height * 4) as usize);
        pixels.fill(0);
        DrawIconEx(dc, 0, 0, HICON(cursor.0), width, height, 0, None, DI_NORMAL)?;
        let _ = GdiFlush();
        let black = pixels.to_vec();
        pixels.fill(255);
        DrawIconEx(dc, 0, 0, HICON(cursor.0), width, height, 0, None, DI_NORMAL)?;
        let _ = GdiFlush();
        let mut rgba = Vec::with_capacity(pixels.len());
        for (b, w) in black.chunks_exact(4).zip(pixels.chunks_exact(4)) {
            let alpha = 255 - (0..3).map(|i| w[i].saturating_sub(b[i])).max().unwrap();
            for i in [2, 1, 0] {
                rgba.push(if alpha == 0 {
                    0
                } else {
                    (u32::from(b[i]) * 255 / u32::from(alpha)).min(255) as u8
                });
            }
            rgba.push(alpha);
        }
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width as u32, height as u32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&rgba)?;
        }
        Ok(CursorImage {
            png: base64::engine::general_purpose::STANDARD.encode(bytes),
            x: info.0.xHotspot.min(width as u32 - 1),
            y: info.0.yHotspot.min(height as u32 - 1),
        })
    }
}
