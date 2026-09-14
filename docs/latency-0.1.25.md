# 0.1.25 hybrid desktop transport

User feedback on 0.1.24 remained poor. Actual connection logs recorded up to
1.7 seconds of tile commit RTT despite capture/encode maxima around 46 ms.
The previous bounded PNG pipeline still queued too much reliable data.

## Changes

- H.264 remains running while lossless refinement is negotiated.
- NVENC VBR/CQ 18 caps motion at min(configured bitrate, 4 Mbps), with a
  two-frame VBV; legacy video-only clients keep their existing settings.
- Remove the second live-video sender pacing clock, whose scheduler overruns
  otherwise accumulate behind an already clocked capture/encoder.
- PNG/copy updates are idle-only, one update in flight, 8 KiB chunks and
  a 16 KiB outstanding threshold (one chunk can overshoot to 24 KiB).
- New input stops further refinement chunks. Partial updates are discarded
  without losing the previous delta baseline. Reliable keyboard input is kept.
- Decode completion does not display a snapshot. Host re-capture validates it
  after the ACK, and a timestamp watermark rejects shows older than local input.
  Local input hides refinement immediately; remote damage invalidates it.
- Refinement failure leaves the bounded video stream running.

## Validation

Browser fixture verifies exact RGB damage/copy reconstruction, cancellation,
resuming from the correct predecessor, stale show rejection, immediate local
invalidation, remote invalidation, resize and malformed-message cleanup.

Isolated live Windows desktop with current Host and embedded Web, Chrome at
2000x1250, 32 seconds: 1918 decoded frames (59.9375 FPS), 1.93 Mbps average,
0.31 ms mean decode time, 34.81 ms mean jitter-buffer residence. Input ACK RTT
median was 0.6 ms both initially and after 32 seconds; p95 after 32 seconds was
0.9 ms on both input channels. Video continued after refinement channel close,
and independent input peer failure switched to WebSocket control.
Evidence: `.run/hybrid-console.log`.

This run's desktop kept changing; it completed no idle refinements. The injected
100 ms refinement-ACK delay therefore had no effect. It does not establish
lossless refinement throughput on a WAN. Pixel/protocol tests cover refinement
correctness separately. A prior run was invalidated by a disconnected Windows
session and DXGI/SendInput failures; the session was restored before this run.

Synthetic 2560x1600 motion using the actual Rust hybrid NVENC arguments:
360/360 frames decoded, 4.001 Mbps including container overhead, encoder throughput
199 FPS. Evidence: `.run/hybrid-motion.log`. Command:
`python scripts/measure_ffmpeg_stability.py --ffmpeg <ffmpeg.exe> --hybrid --only production_motion`.

Packaged release binaries also passed the same test: 1926 frames over the
32-second interval (60.19 FPS; sampling boundaries can include a few extra
frames), 2.04 Mbps, 0.41 ms mean decode time, 20.87 ms jitter-buffer residence,
and sustained input median 0.4 ms / p95 0.5 ms. This run also completed no idle
refinement because desktop pixels kept changing. Evidence:
`.run/hybrid-final-0.1.25.log`.

## Limits

These are loopback tests, not RDP comparisons or input-to-visible measurements.
The actual proxy path needs user validation after installation. 4 Mbps is an
initial ceiling, not congestion-adaptive bandwidth estimation. A slower link
can still queue video. Motion is lossy; fine moving text can soften. PNG is
lossless relative to 8-bit GDI RGB. Continuously changing desktops may never
qualify for a whole-desktop idle refinement. No HDR preservation is claimed.
