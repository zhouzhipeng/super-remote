//! Idle-only lossless refinements over a continuously running video stream.
use bytes::BytesMut;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use windows::Win32::{Graphics::Gdi::*, UI::WindowsAndMessaging::*};

const TILE: usize = 128;
const MAX_PIXELS: usize = 8192 * 4320;

#[derive(Clone, Debug, serde::Serialize)]
struct CopyRect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    source_y: usize,
}

// Motion estimation is only a hint. Every byte of a copied tile is verified
// afterwards, so repeated backgrounds or ambiguous text cannot corrupt pixels.
fn scroll_offset(previous: &Desktop, current: &Desktop) -> Option<isize> {
    let mut anchors = Vec::new();
    for y in (16..current.height.saturating_sub(16)).step_by(19) {
        for x in (0..current.width.saturating_sub(2)).step_by(11) {
            let i = (y * current.width + x) * 3;
            if current.rgb[i..i + 3] != current.rgb[i + 3..i + 6]
                && current.rgb[i..i + 6] != previous.rgb[i..i + 6]
            {
                anchors.push((x, y));
                break;
            }
        }
        if anchors.len() == 32 {
            break;
        }
    }
    if anchors.len() < 4 {
        return None;
    }
    let mut best = (0, 0);
    for dy in -512isize..=512 {
        if dy == 0 {
            continue;
        }
        let score = anchors
            .iter()
            .filter(|&&(x, y)| {
                let sy = y as isize + dy;
                if sy < 0 || sy >= current.height as isize {
                    return false;
                }
                let a = (y * current.width + x) * 3;
                let b = (sy as usize * current.width + x) * 3;
                current.rgb[a..a + 6] == previous.rgb[b..b + 6]
            })
            .count();
        if score > best.1 {
            best = (dy, score);
        }
    }
    (best.1 >= 4 && best.1 * 2 >= anchors.len()).then_some(best.0)
}

#[derive(PartialEq, Eq)]
struct Desktop {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

fn capture() -> anyhow::Result<Desktop> {
    unsafe {
        // Tokio's blocking worker can have a different DPI context from main.
        // Capture physical pixels, never a DPI-virtualized desktop bitmap.
        use windows::Win32::UI::HiDpi::{
            DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            SetThreadDpiAwarenessContext,
        };
        struct DpiGuard(DPI_AWARENESS_CONTEXT);
        impl Drop for DpiGuard {
            fn drop(&mut self) {
                unsafe {
                    SetThreadDpiAwarenessContext(self.0);
                }
            }
        }
        let _dpi = DpiGuard(SetThreadDpiAwarenessContext(
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        ));
        let width = GetSystemMetrics(SM_CXSCREEN);
        let height = GetSystemMetrics(SM_CYSCREEN);
        anyhow::ensure!(
            width > 0
                && width <= 8192
                && height > 0
                && height <= 4320
                && width as usize * height as usize <= MAX_PIXELS,
            "unsupported desktop dimensions"
        );
        struct Surface {
            screen: HDC,
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
                    if !self.dc.is_invalid() {
                        let _ = DeleteDC(self.dc);
                    }
                    if !self.screen.is_invalid() {
                        ReleaseDC(None, self.screen);
                    }
                }
            }
        }
        let screen = GetDC(None);
        anyhow::ensure!(!screen.is_invalid(), "desktop DC unavailable");
        let mut surface = Surface {
            screen,
            dc: CreateCompatibleDC(Some(screen)),
            bitmap: HBITMAP::default(),
            old: HGDIOBJ::default(),
        };
        anyhow::ensure!(!surface.dc.is_invalid(), "capture DC unavailable");
        let info = BITMAPINFO {
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
        surface.bitmap =
            CreateDIBSection(Some(surface.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        surface.old = SelectObject(surface.dc, surface.bitmap.into());
        BitBlt(
            surface.dc,
            0,
            0,
            width,
            height,
            Some(screen),
            0,
            0,
            SRCCOPY | CAPTUREBLT,
        )?;
        GdiFlush().ok()?;
        let bgra =
            std::slice::from_raw_parts(bits.cast::<u8>(), width as usize * height as usize * 4);
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for pixel in bgra.chunks_exact(4) {
            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
        Ok(Desktop {
            width: width as usize,
            height: height as usize,
            rgb,
        })
    }
}

fn changed_tiles(
    previous: Option<&Desktop>,
    current: &Desktop,
) -> anyhow::Result<(Vec<u8>, usize, Vec<CopyRect>)> {
    let previous = previous.filter(|p| p.width == current.width && p.height == current.height);
    let mut output = Vec::new();
    let mut count = 0;
    let mut copies = Vec::new();
    let motion = previous.and_then(|p| scroll_offset(p, current));
    for y in (0..current.height).step_by(TILE) {
        for x in (0..current.width).step_by(TILE) {
            let width = TILE.min(current.width - x);
            let height = TILE.min(current.height - y);
            let row = |dy: usize| ((y + dy) * current.width + x) * 3;
            if previous.is_some_and(|p| {
                (0..height).all(|dy| {
                    let start = row(dy);
                    p.rgb[start..start + width * 3] == current.rgb[start..start + width * 3]
                })
            }) {
                continue;
            }
            if let (Some(p), Some(dy)) = (previous, motion) {
                let source_y = y as isize + dy;
                if copies.len() < 512
                    && source_y >= 0
                    && source_y as usize + height <= current.height
                    && (0..height).all(|row| {
                        let a = ((y + row) * current.width + x) * 3;
                        let b = ((source_y as usize + row) * current.width + x) * 3;
                        current.rgb[a..a + width * 3] == p.rgb[b..b + width * 3]
                    })
                {
                    copies.push(CopyRect {
                        x,
                        y,
                        width,
                        height,
                        source_y: source_y as usize,
                    });
                    continue;
                }
            }
            let mut rgb = Vec::with_capacity(width * height * 3);
            for dy in 0..height {
                let start = row(dy);
                rgb.extend_from_slice(&current.rgb[start..start + width * 3]);
            }
            let mut png = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut png, width as u32, height as u32);
                encoder.set_color(png::ColorType::Rgb);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.set_compression(png::Compression::Fast);
                encoder.write_header()?.write_image_data(&rgb)?;
            }
            for value in [x, y, width, height] {
                output.extend_from_slice(&(value as u16).to_le_bytes());
            }
            output.extend_from_slice(&(png.len() as u32).to_le_bytes());
            output.extend(png);
            count += 1;
        }
    }
    Ok((output, count, copies))
}

pub async fn serve(
    channel: Arc<dyn DataChannel>,
    retired: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    input: Arc<std::sync::Mutex<crate::input::SessionInput>>,
) -> anyhow::Result<()> {
    let _display = crate::display_power::DisplayPowerGuard::acquire()?;
    let (ack_tx, mut ack_rx) = tokio::sync::mpsc::channel::<anyhow::Result<u32>>(16);
    let ack_channel = channel.clone();
    struct AckTask(tokio::task::JoinHandle<()>);
    impl Drop for AckTask {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _ack_task = AckTask(tokio::spawn(async move {
        while let Some(event) = ack_channel.poll().await {
            match event {
                DataChannelEvent::OnMessage(message) if message.is_string => {
                    let ack = (|| -> anyhow::Result<u32> {
                        let value: serde_json::Value = serde_json::from_slice(&message.data)?;
                        anyhow::ensure!(value["type"] == "ack", "invalid desktop ACK");
                        let id = value["id"]
                            .as_u64()
                            .and_then(|v| u32::try_from(v).ok())
                            .ok_or_else(|| anyhow::anyhow!("invalid desktop sequence"))?;
                        Ok(id)
                    })();
                    if ack_tx.send(ack).await.is_err() {
                        return;
                    }
                }
                DataChannelEvent::OnClose | DataChannelEvent::OnError => return,
                _ => {}
            }
        }
    }));
    let mut previous: Option<Desktop> = None;
    let mut observed: Option<Desktop> = None;
    let mut stable_since = std::time::Instant::now();
    let mut id = 0u32;
    let mut committed = 0u32;
    let mut shown = false;
    let activity = || input.lock().unwrap_or_else(|e| e.into_inner()).activity();
    loop {
        if retired.load(Ordering::Acquire) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        anyhow::ensure!(!ack_rx.is_closed(), "desktop channel closed");
        if !active.load(Ordering::Acquire) {
            continue;
        }
        let current = tokio::task::spawn_blocking(capture).await??;
        let (watermark, idle) = activity();
        let changed = observed.as_ref() != Some(&current);
        if changed || idle < Duration::from_millis(450) {
            stable_since = std::time::Instant::now();
            if shown {
                channel.send_text("{\"type\":\"invalidate\"}").await?;
                shown = false;
            }
        }
        observed = Some(current);
        if stable_since.elapsed() < Duration::from_millis(300) || idle < Duration::from_millis(450)
        {
            continue;
        }
        if previous.as_ref() == observed.as_ref() {
            if !shown && committed != 0 {
                channel
                    .send_text(
                        &serde_json::json!({"type":"show", "id":committed,
                    "input":watermark.to_string()})
                        .to_string(),
                    )
                    .await?;
                shown = true;
            }
            continue;
        }
        let current = observed.take().unwrap();
        let (old, current, payload, count, copies) =
            tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                let (payload, count, copies) = changed_tiles(previous.as_ref(), &current)?;
                Ok((previous, current, payload, count, copies))
            })
            .await??;
        previous = old;
        if activity().0 != watermark {
            observed = Some(current);
            continue;
        }
        id = id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("desktop sequence exhausted"))?;
        channel
            .send_text(
                &serde_json::json!({"type":"begin","id":id,
            "width":current.width,"height":current.height,"bytes":payload.len(),
            "tiles":count,"copies":copies})
                .to_string(),
            )
            .await?;
        let started = std::time::Instant::now();
        let completed = tokio::time::timeout(Duration::from_secs(30), async {
            for chunk in payload.chunks(8 * 1024) {
                loop {
                    anyhow::ensure!(!retired.load(Ordering::Acquire), "session retired");
                    // Abandon refinement on new input before adding more reliable
                    // traffic. At most 24 KiB can be ahead of the cancellation.
                    if activity().0 != watermark {
                        return anyhow::Ok(false);
                    }
                    if channel.outstanding_bytes().await? <= 16 * 1024 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                channel.send(BytesMut::from(chunk)).await?;
            }
            anyhow::Ok(true)
        })
        .await??;
        if !completed {
            channel
                .send_text(&serde_json::json!({"type":"cancel", "id":id}).to_string())
                .await?;
            observed = Some(current);
            stable_since = std::time::Instant::now();
            continue;
        }
        channel
            .send_text(&serde_json::json!({"type":"end", "id":id}).to_string())
            .await?;
        let ack = tokio::time::timeout(Duration::from_secs(10), ack_rx.recv())
            .await?
            .ok_or_else(|| anyhow::anyhow!("desktop channel closed"))??;
        anyhow::ensure!(ack == id, "unexpected refinement ACK");
        tracing::info!(
            id,
            bytes = payload.len(),
            tiles = count,
            copied_tiles = copies.len(),
            commit_rtt_ms = started.elapsed().as_millis(),
            "idle desktop refinement committed"
        );
        committed = id;
        previous = Some(current);
        // Re-capture and validate AFTER transfer and ACK. A completed but stale
        // snapshot stays hidden; its pixels remain a valid delta baseline.
        observed = None;
        stable_since = std::time::Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scrolling_reuses_pixels_and_reconstructs_every_rgb_byte() {
        let (width, height) = (384usize, 512usize);
        let mut rgb = Vec::with_capacity(width * height * 3);
        let mut seed = 17u32;
        for _ in 0..width * height * 3 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            rgb.push(seed as u8);
        }
        let before = Desktop { width, height, rgb };
        let mut after = Desktop {
            width,
            height,
            rgb: vec![249; width * height * 3],
        };
        after.rgb[..(height - 37) * width * 3].copy_from_slice(&before.rgb[37 * width * 3..]);
        let (payload, count, copies) = changed_tiles(Some(&before), &after).unwrap();
        assert_eq!(copies.len(), 9);
        assert_eq!(count, 3);
        let mut actual = before.rgb.clone();
        for copy in copies {
            for row in 0..copy.height {
                let dst = ((copy.y + row) * width + copy.x) * 3;
                let src = ((copy.source_y + row) * width + copy.x) * 3;
                actual[dst..dst + copy.width * 3]
                    .copy_from_slice(&before.rgb[src..src + copy.width * 3]);
            }
        }
        let mut offset = 0;
        for _ in 0..count {
            let u16_at = |i| u16::from_le_bytes(payload[i..i + 2].try_into().unwrap()) as usize;
            let (x, y, w, h) = (
                u16_at(offset),
                u16_at(offset + 2),
                u16_at(offset + 4),
                u16_at(offset + 6),
            );
            let size =
                u32::from_le_bytes(payload[offset + 8..offset + 12].try_into().unwrap()) as usize;
            let mut decoder = png::Decoder::new(&payload[offset + 12..offset + 12 + size])
                .read_info()
                .unwrap();
            let mut pixels = vec![0; decoder.output_buffer_size()];
            decoder.next_frame(&mut pixels).unwrap();
            for row in 0..h {
                let dst = ((y + row) * width + x) * 3;
                actual[dst..dst + w * 3].copy_from_slice(&pixels[row * w * 3..(row + 1) * w * 3]);
            }
            offset += 12 + size;
        }
        assert_eq!(actual, after.rgb);
        let full = changed_tiles(None, &after).unwrap().0.len();
        assert!(
            payload.len() * 2 < full,
            "scroll damage should materially reduce wire bytes"
        );
    }
    #[test]
    #[ignore = "captures the current primary desktop; run explicitly for DPI validation"]
    fn capture_uses_physical_desktop_dimensions() {
        let desktop = capture().unwrap();
        unsafe {
            let mut mode = DEVMODEW::default();
            mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
            assert!(EnumDisplaySettingsW(None, ENUM_CURRENT_SETTINGS, &mut mode).as_bool());
            assert_eq!(
                (desktop.width, desktop.height),
                (mode.dmPelsWidth as usize, mode.dmPelsHeight as usize)
            );
        }
    }
    #[test]
    fn exact_damage_and_edge_tiles_round_trip_losslessly() {
        let before = Desktop {
            width: 130,
            height: 129,
            rgb: vec![17; 130 * 129 * 3],
        };
        assert_eq!(changed_tiles(Some(&before), &before).unwrap().1, 0);
        let mut after = Desktop {
            width: 130,
            height: 129,
            rgb: before.rgb.clone(),
        };
        let offset = (128 * 130 + 129) * 3;
        after.rgb[offset..offset + 3].copy_from_slice(&[1, 2, 255]);
        let (data, count, _) = changed_tiles(Some(&before), &after).unwrap();
        assert_eq!(count, 1);
        assert_eq!(&data[..8], &[128, 0, 128, 0, 2, 0, 1, 0]);
        let mut decoder = png::Decoder::new(&data[12..]).read_info().unwrap();
        let mut rgb = vec![0; decoder.output_buffer_size()];
        decoder.next_frame(&mut rgb).unwrap();
        assert_eq!(rgb, [17, 17, 17, 1, 2, 255]);
        assert_eq!(changed_tiles(None, &after).unwrap().1, 4);
    }
}
