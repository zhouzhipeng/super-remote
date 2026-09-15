//! Lossless refinements over a continuously running video stream.
//!
//! The refinement worker runs continuously, including while the user is
//! interacting. Only *presentation* of the sharp layer is gated on input
//! (`SessionInput::presentation_ready`): suspending capture as well used to
//! leave the delta baseline frozen at the pre-interaction screen, so the first
//! update after a scroll could not reuse any pixels and degenerated into a
//! whole-desktop re-encode exactly when the user was waiting for it.
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
/// Capture cadence while the sharp layer is presentable.
const IDLE_INTERVAL: Duration = Duration::from_millis(16);
/// Capture cadence while the user is interacting. Updates keep flowing so the
/// baseline (and its motion estimate) stays anchored to the live screen, but
/// rarely enough to leave the interaction video its bandwidth. Measured from
/// the *end* of the previous refinement, so a slow link throttles itself.
///
/// The sharp layer is not presented while interacting, so every byte spent here
/// is taken from the video stream that *is* being watched - during precisely the
/// moments the user is judging whether scrolling is smooth. This interval and
/// `INTERACTION_TILE_LIMIT` together are a bandwidth budget, not just a size cap:
/// at roughly 6 KiB for a tile of text they keep refinement near 1 Mbit/s.
const INTERACTION_INTERVAL: Duration = Duration::from_millis(600);
/// PNG tiles encoded and sent in one update while interacting. A scroll is
/// copy rectangles - which cost almost nothing - plus the newly exposed band;
/// this admits that band and declines anything that has stopped being a
/// translation. Checked before encoding, so a rejected update costs damage
/// detection only. Keeping it small matters more than keeping the baseline
/// perfectly fresh: an older baseline costs one larger delta once the scroll
/// ends, while an oversized one costs the video stream throughout it.
const INTERACTION_TILE_LIMIT: usize = 24;
/// Reliable bytes allowed in flight for one refinement. Refinement shares the
/// media PeerConnection; input has its own transport, so this bounds only how
/// much of a superseded update can still be on the wire - and how long queued
/// refinement can delay video packets behind it. The previous 16 KiB capped
/// throughput at roughly 16 KiB/RTT, which is what made a post-scroll recovery
/// take seconds across a relay; this saturates any realistic relay while still
/// draining in well under a second.
const INFLIGHT_BYTES: usize = 192 * 1024;
/// One reliable message. 16 KiB is the interoperable SCTP ceiling.
const CHUNK: usize = 16 * 1024;
/// New input abandons an in-flight update only while at least this much is
/// still unsent. Finishing a nearly complete transfer keeps the baseline warm
/// for less than it costs to redo it.
const CANCEL_REMAINDER: usize = 256 * 1024;
/// A capture taken immediately after injection can still show the pre-input
/// screen, and presenting that is the "jump back to the old frame" artefact.
/// Require the scene to hold still against the baseline for this long before
/// the baseline may certify the current input state.
const STABLE_CONFIRM: Duration = Duration::from_millis(60);
/// Vertical motion search range. One skipped or slow capture during a fast
/// scroll must not push the real offset outside the window and force every
/// tile to be re-encoded.
const MOTION_RANGE: isize = 1024;
/// Below this, spawning encoder threads costs more than it saves.
const PARALLEL_TILE_THRESHOLD: usize = 8;
/// A refinement cycle slower than this cannot present motion: it caps the sharp
/// layer at under 7 updates per second.
const MOTION_CYCLE: Duration = Duration::from_millis(150);
/// Consecutive cycles that were slow, large and already out of date on arrival
/// before the scene counts as animating. One large change followed by a still
/// screen - a window opening - never reaches this.
const MOTION_STREAK: u32 = 3;
/// An update must also re-encode at least this share of the desktop's tiles to
/// count as motion. A video window is tens of tiles every frame; a keystroke or
/// a caret is one or two, so typing keeps its sharp incremental updates even
/// where the link is slow enough to make every cycle exceed `MOTION_CYCLE`.
const MOTION_TILE_SHARE: usize = 8;
/// While the scene is animating the sharp layer is not presented, so refinement
/// only has to keep a baseline ready for when motion stops. Backing off this
/// far stops full updates from competing for the link with the video stream the
/// user is actually watching.
const MOTION_INTERVAL: Duration = Duration::from_millis(1000);

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
    for dy in -MOTION_RANGE..=MOTION_RANGE {
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

/// `rgb` is a buffer recycled from a retired `Desktop`. A fresh 3-byte-per-pixel
/// allocation every tick churns tens of megabytes per second at 4K.
fn capture(mut rgb: Vec<u8>) -> anyhow::Result<Desktop> {
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
        // Size the buffer once per resolution; every byte below is overwritten.
        let needed = width as usize * height as usize * 3;
        if rgb.len() != needed {
            rgb.clear();
            rgb.resize(needed, 0);
        }
        for (destination, pixel) in rgb.chunks_exact_mut(3).zip(bgra.chunks_exact(4)) {
            destination[0] = pixel[2];
            destination[1] = pixel[1];
            destination[2] = pixel[0];
        }
        Ok(Desktop {
            width: width as usize,
            height: height as usize,
            rgb,
        })
    }
}

/// One tile's wire record: a 12-byte header followed by its PNG.
fn encode_tile(
    current: &Desktop,
    (x, y, width, height): (usize, usize, usize, usize),
) -> anyhow::Result<Vec<u8>> {
    let mut rgb = Vec::with_capacity(width * height * 3);
    for dy in 0..height {
        let start = ((y + dy) * current.width + x) * 3;
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
    let mut record = Vec::with_capacity(12 + png.len());
    for value in [x, y, width, height] {
        record.extend_from_slice(&(value as u16).to_le_bytes());
    }
    record.extend_from_slice(&(png.len() as u32).to_le_bytes());
    record.extend_from_slice(&png);
    Ok(record)
}

/// PNG compression dominates a large update and every tile is independent, so
/// a whole-desktop refinement is otherwise one core's serial work while the
/// browser waits. Source order is preserved; the scoped threads cannot outlive
/// the borrow of `current`.
fn encode_tiles(
    current: &Desktop,
    pending: &[(usize, usize, usize, usize)],
) -> anyhow::Result<Vec<Vec<u8>>> {
    if pending.len() < PARALLEL_TILE_THRESHOLD {
        return pending.iter().map(|&tile| encode_tile(current, tile)).collect();
    }
    let threads = std::thread::available_parallelism()
        .map_or(4, |value| value.get())
        .clamp(1, pending.len());
    let batch = pending.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let workers: Vec<_> = pending
            .chunks(batch)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|&tile| encode_tile(current, tile))
                        .collect::<anyhow::Result<Vec<_>>>()
                })
            })
            .collect();
        let mut encoded = Vec::with_capacity(pending.len());
        for worker in workers {
            encoded.extend(
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("desktop tile encoder panicked"))??,
            );
        }
        anyhow::Ok(encoded)
    })
}

/// Returns `None` when more than `tile_limit` tiles would have to be encoded.
/// The decision is made from damage detection alone: encoding a whole desktop
/// only to discard it would cost more than the update it declines.
fn changed_tiles(
    previous: Option<&Desktop>,
    current: &Desktop,
    tile_limit: usize,
) -> anyhow::Result<Option<(Vec<u8>, usize, Vec<CopyRect>)>> {
    let previous = previous.filter(|p| p.width == current.width && p.height == current.height);
    let mut copies = Vec::new();
    let mut pending = Vec::new();
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
            pending.push((x, y, width, height));
        }
    }
    if pending.len() > tile_limit {
        return Ok(None);
    }
    let encoded = encode_tiles(current, &pending)?;
    let mut output = Vec::with_capacity(encoded.iter().map(Vec::len).sum());
    for record in &encoded {
        output.extend_from_slice(record);
    }
    Ok(Some((output, encoded.len(), copies)))
}

/// Tiles one full refinement of this desktop would carry.
fn desktop_tiles(desktop: &Desktop) -> usize {
    desktop.width.div_ceil(TILE) * desktop.height.div_ceil(TILE)
}

/// Whether the screen still matches the committed baseline. The per-tile
/// refinement mask is no longer transmitted, so the loop only needs the answer,
/// not the list - and a whole-buffer comparison stops at the first difference
/// instead of scanning every remaining tile.
fn unchanged_desktop(previous: &Desktop, current: &Desktop) -> bool {
    previous.width == current.width
        && previous.height == current.height
        && previous.rgb == current.rgb
}

// Crossing from live video back to PNG must not replay an earlier layout.
// Once on the sharp path, ordered deltas remain authoritative (typing/automatic
// changes keep their established sharpness policy). Tolerate only caret-sized
// differences during recovery, never a different window/fullscreen layout.
fn can_restore_snapshot(shown: bool, baseline: &Desktop, current: &Desktop) -> bool {
    if baseline.width != current.width || baseline.height != current.height { return false; }
    shown || baseline.rgb.chunks_exact(3).zip(current.rgb.chunks_exact(3))
        .filter(|(a,b)| a != b).take(513).count() <= 512
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
    // Retired pixel buffers are handed back here instead of being reallocated.
    let mut spare: Vec<u8> = Vec::new();
    let mut last_refinement = std::time::Instant::now() - Duration::from_secs(1);
    let mut last_mask: Option<(u32, u64, Vec<usize>)> = None;
    let mut id = 0u32;
    let mut committed = 0u32;
    let mut committed_input = None;
    let mut shown = false;
    let mut stable_since: Option<std::time::Instant> = None;
    // Consecutive updates that were stale before they could be presented, and
    // the cost and size of the most recent one. Together they decide whether the
    // scene is changing faster than this path can represent it.
    let mut behind = 0u32;
    let mut last_cycle = Duration::ZERO;
    let mut last_tiles = 0usize;
    let state = || {
        let input = input.lock().unwrap_or_else(|e| e.into_inner());
        (input.activity().0, input.presentation_ready())
    };
    loop {
        if retired.load(Ordering::Acquire) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(16)).await;
        anyhow::ensure!(!ack_rx.is_closed(), "desktop channel closed");
        if !active.load(Ordering::Acquire) {
            continue;
        }
        let (watermark, presentable) = state();
        // Video playback, and any other continuously changing scene, updates
        // faster than capture -> encode -> transfer -> ACK can deliver. Holding
        // the sharp layer over it turns smooth playback into a slideshow, and
        // `can_restore_snapshot` keeps it there because an already-shown layer
        // is never re-validated. Hand the scene to H.264, which exists for
        // exactly this, until it settles.
        let animating = behind >= MOTION_STREAK;
        if !presentable || animating {
            // The sharp layer cannot claim to be a picture of now, so retract it.
            // Refinement itself continues below: a baseline left frozen for the
            // whole interaction is what turns the first update after a scroll
            // into a whole-desktop re-encode.
            if shown {
                channel.send_text("{\"type\":\"invalidate\"}").await?;
                shown = false;
            }
            last_mask = None;
            stable_since = None;
        }
        let interval = if animating {
            MOTION_INTERVAL
        } else if presentable {
            IDLE_INTERVAL
        } else {
            INTERACTION_INTERVAL
        };
        if last_refinement.elapsed() < interval {
            continue;
        }
        let cycle_started = std::time::Instant::now();
        let reuse = std::mem::take(&mut spare);
        let current = tokio::task::spawn_blocking(move || capture(reuse)).await??;
        let mut unchanged = false;
        if let Some(baseline) = &previous {
            unchanged = unchanged_desktop(baseline, &current);
            let mask = (committed, watermark, Vec::<usize>::new());
            if presentable
                && !animating
                && committed_input == Some(watermark)
                && can_restore_snapshot(shown, baseline, &current)
                && last_mask.as_ref() != Some(&mask) {
                channel
                    .send_text(
                        &serde_json::json!({"type":"show", "id":committed,
                    "input":watermark.to_string(), "hidden":mask.2})
                        .to_string(),
                    )
                    .await?;
                last_mask = Some(mask);
                shown = true;
            }
        }
        if unchanged {
            // The scene caught up with its baseline, so nothing is outrunning
            // refinement any more and the sharp layer may be earned back.
            behind = 0;
            last_cycle = Duration::ZERO;
            last_tiles = 0;
            // A single capture can be taken before the application has repainted
            // the input that was just injected, and certifying the baseline from
            // that capture is what allowed a pre-scroll frame to be shown again.
            // Require the scene to hold still first; a commit certifies its own
            // watermark below, so this only gates the no-op path.
            let since = *stable_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() >= STABLE_CONFIRM {
                committed_input = Some(watermark);
            }
            spare = current.rgb;
            continue;
        }
        // The previous update was already out of date when this capture arrived,
        // and it was too slow and too large to have been motion this path could
        // have presented anyway.
        if last_cycle >= MOTION_CYCLE && last_tiles * MOTION_TILE_SHARE >= desktop_tiles(&current) {
            behind = behind.saturating_add(1);
        }
        stable_since = None;
        // A first baseline is never optional: without it the browser has no
        // pixels at all, so it is sent whatever the user is doing.
        let tile_limit = if presentable || previous.is_none() {
            usize::MAX
        } else {
            INTERACTION_TILE_LIMIT
        };
        let (old, current, update) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let update = changed_tiles(previous.as_ref(), &current, tile_limit)?;
            Ok((previous, current, update))
        })
        .await??;
        previous = old;
        let Some((payload, count, copies)) = update else {
            // A whole repaint rather than a scroll delta. Leave the link to the
            // video stream and retry once the interaction window closes.
            spare = current.rgb;
            last_refinement = std::time::Instant::now();
            continue;
        };
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
            let mut remaining = payload.len();
            for chunk in payload.chunks(CHUNK) {
                anyhow::ensure!(!retired.load(Ordering::Acquire), "session retired");
                // A committed delta is a real historical screen state and
                // presentation is gated separately, so new input no longer
                // invalidates it. Only abandon an update whose remaining bytes
                // would keep competing with the interaction video. Checked once
                // per chunk: the input mutex is also held by injection on its
                // high-priority thread and must not be contended in a spin.
                if remaining > CANCEL_REMAINDER && state().0 != watermark {
                    return anyhow::Ok(false);
                }
                while channel.outstanding_bytes().await? > INFLIGHT_BYTES {
                    anyhow::ensure!(!retired.load(Ordering::Acquire), "session retired");
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                channel.send(BytesMut::from(chunk)).await?;
                remaining -= chunk.len();
            }
            anyhow::Ok(true)
        })
        .await??;
        if !completed {
            channel
                .send_text(&serde_json::json!({"type":"cancel", "id":id}).to_string())
                .await?;
            spare = current.rgb;
            last_refinement = std::time::Instant::now();
            continue;
        }
        channel
            .send_text(&serde_json::json!({"type":"end", "id":id}).to_string())
            .await?;
        let ack = tokio::time::timeout(Duration::from_secs(10), ack_rx.recv())
            .await?
            .ok_or_else(|| anyhow::anyhow!("desktop channel closed"))??;
        anyhow::ensure!(ack == id, "unexpected refinement ACK");
        // Capture through browser ACK: what one sharp update actually costs.
        let cycle = cycle_started.elapsed();
        tracing::info!(
            id,
            bytes = payload.len(),
            tiles = count,
            copied_tiles = copies.len(),
            commit_rtt_ms = started.elapsed().as_millis(),
            cycle_ms = cycle.as_millis(),
            interacting = !presentable,
            animating,
            "desktop refinement committed"
        );
        last_cycle = cycle;
        last_tiles = count;
        committed = id;
        committed_input = Some(watermark);
        // Re-capture and validate AFTER transfer and ACK. A completed but stale
        // snapshot stays hidden; its pixels remain a valid delta baseline.
        if let Some(retired_baseline) = previous.replace(current) {
            spare = retired_baseline.rgb;
        }
        last_mask = None;
        last_refinement = std::time::Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fullscreen_transition_cannot_restore_an_intermediate_snapshot() {
        let windowed = Desktop { width: 256, height: 128, rgb: vec![30; 256 * 128 * 3] };
        let fullscreen = Desktop { width: 256, height: 128, rgb: vec![220; 256 * 128 * 3] };
        assert!(!can_restore_snapshot(false, &windowed, &fullscreen));
        assert!(can_restore_snapshot(false, &fullscreen, &fullscreen));
        // Reverse transition must reject the old fullscreen snapshot too.
        assert!(!can_restore_snapshot(false, &fullscreen, &windowed));
        let mut caret = Desktop { width: 256, height: 128, rgb: fullscreen.rgb.clone() };
        caret.rgb[..80 * 3].fill(0);
        assert!(can_restore_snapshot(false, &fullscreen, &caret));
        // Existing sharp-only automatic updates are not forced onto low-res video.
        assert!(can_restore_snapshot(true, &windowed, &fullscreen));
    }

    #[test]
    fn a_single_changed_pixel_or_a_resize_retires_the_baseline() {
        let before = Desktop {
            width: 256,
            height: 129,
            rgb: vec![42; 256 * 129 * 3],
        };
        let mut after = Desktop {
            width: 256,
            height: 129,
            rgb: before.rgb.clone(),
        };
        assert!(unchanged_desktop(&before, &after));
        after.rgb[(128 * 256 + 255) * 3] = 43;
        assert!(!unchanged_desktop(&before, &after));
        after.rgb[(128 * 256 + 255) * 3] = 42;
        after.width = 128;
        assert!(!unchanged_desktop(&before, &after));
    }

    #[test]
    fn motion_is_distinguished_from_typing_and_from_one_off_changes() {
        // A 1920x1080 desktop carries 15 x 9 = 135 tiles.
        let desktop = Desktop { width: 1920, height: 1080, rgb: vec![0; 1920 * 1080 * 3] };
        assert_eq!(desktop_tiles(&desktop), 135);
        let motion = |cycle: Duration, tiles: usize| {
            cycle >= MOTION_CYCLE && tiles * MOTION_TILE_SHARE >= desktop_tiles(&desktop)
        };
        // A video window re-encoding a quarter of the screen too slowly to present.
        assert!(motion(Duration::from_millis(300), 34));
        // A keystroke or caret on a link slow enough to exceed the cycle anyway
        // must keep its sharp incremental updates.
        assert!(!motion(Duration::from_millis(300), 2));
        // A large change this path can still deliver in time is not motion.
        assert!(!motion(Duration::from_millis(40), 135));
        // Exactly one eighth qualifies; just under does not.
        assert!(motion(MOTION_CYCLE, 17));
        assert!(!motion(MOTION_CYCLE, 16));
    }

    #[test]
    fn parallel_and_serial_tile_encoding_produce_identical_bytes() {
        let (width, height) = (512usize, 384usize);
        let mut rgb = Vec::with_capacity(width * height * 3);
        let mut seed = 99u32;
        for _ in 0..width * height * 3 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            rgb.push(seed as u8);
        }
        let desktop = Desktop { width, height, rgb };
        let pending: Vec<_> = (0..height)
            .step_by(TILE)
            .flat_map(|y| {
                (0..width)
                    .step_by(TILE)
                    .map(move |x| (x, y, TILE.min(width - x), TILE.min(height - y)))
            })
            .collect();
        assert!(pending.len() >= PARALLEL_TILE_THRESHOLD);
        let parallel = encode_tiles(&desktop, &pending).unwrap();
        let serial: Vec<_> = pending
            .iter()
            .map(|&tile| encode_tile(&desktop, tile).unwrap())
            .collect();
        assert_eq!(parallel, serial);
        // The threshold path must agree on a batch too small to be split.
        let small = &pending[..PARALLEL_TILE_THRESHOLD - 1];
        assert_eq!(encode_tiles(&desktop, small).unwrap(), serial[..small.len()]);
        assert!(encode_tiles(&desktop, &[]).unwrap().is_empty());
    }

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
        let (payload, count, copies) = changed_tiles(Some(&before), &after, usize::MAX)
            .unwrap()
            .expect("an unlimited update is always produced");
        assert_eq!(copies.len(), 9);
        assert_eq!(count, 3);
        // Damage detection alone decides an over-limit update; nothing is encoded.
        assert!(changed_tiles(Some(&before), &after, 2).unwrap().is_none());
        assert!(changed_tiles(Some(&before), &after, 3).unwrap().is_some());
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
        let full = changed_tiles(None, &after, usize::MAX).unwrap().unwrap().0.len();
        assert!(
            payload.len() * 2 < full,
            "scroll damage should materially reduce wire bytes"
        );
    }
    #[test]
    #[ignore = "captures the current primary desktop; run explicitly for DPI validation"]
    fn capture_uses_physical_desktop_dimensions() {
        let desktop = capture(Vec::new()).unwrap();
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
        let unlimited = |previous, current| {
            changed_tiles(previous, current, usize::MAX)
                .unwrap()
                .expect("an unlimited update is always produced")
        };
        assert_eq!(unlimited(Some(&before), &before).1, 0);
        let mut after = Desktop {
            width: 130,
            height: 129,
            rgb: before.rgb.clone(),
        };
        let offset = (128 * 130 + 129) * 3;
        after.rgb[offset..offset + 3].copy_from_slice(&[1, 2, 255]);
        let (data, count, _) = unlimited(Some(&before), &after);
        assert_eq!(count, 1);
        assert_eq!(&data[..8], &[128, 0, 128, 0, 2, 0, 1, 0]);
        let mut decoder = png::Decoder::new(&data[12..]).read_info().unwrap();
        let mut rgb = vec![0; decoder.output_buffer_size()];
        decoder.next_frame(&mut rgb).unwrap();
        assert_eq!(rgb, [17, 17, 17, 1, 2, 255]);
        assert_eq!(unlimited(None, &after).1, 4);
    }
}
