# 0.1.28 input-first display

Requested policy: accept very low resolution during any mouse/keyboard input.
Negotiated refinement-v2 clients receive an always-ready H.264 stream at a maximum
640-pixel longest edge and 800 kbps NVENC ceiling, keeping the requested frame rate.
Native capture size is retained. Input immediately hides the sharp overlay; no
per-input encoder restart or resolution renegotiation is necessary. Legacy clients
retain their existing stream. Refinement channel failure restores full-resolution
video so a client cannot become permanently stuck with no sharp path.

After 250 ms without input, full-resolution PNG/copy refinements resume. During
active input RGB capture and PNG encoding are skipped, and new input cancels
further chunks of an in-flight update. One update is in flight, at most one per
300 ms; the outstanding limit remains 16 KiB plus an 8 KiB chunk. 250 ms is the
eligibility threshold, not a guaranteed time to complete a high-resolution image.

Show messages identify changed tiles that must remain transparent over video.
Stable regions are shown without waiting for blinking/animated regions. The
unmasked committed baseline is retained separately, so overlapping scroll copies
never read transparency introduced by a presentation mask. Local input watermark
checks and post-transfer validation prevent old snapshots covering newer input.

## Validation

Browser fixture: exact RGB/copy reconstruction, transparent changed regions,
correct retained baseline after masking, malformed packets, cancellation, and
immediate overlay removal for pointer move/down/up, wheel and key down/up. All
six events produced their expected input messages. Host tests cover dimensions,
bitrate, capture-size preservation, portrait/small displays and tile invalidation.

Packaged release test with isolated Host/signaling and a real desktop: 640x400
video; 1807 frames over a nominal 32-second window (56.47 FPS), 0.393 Mbps mean,
0.442 ms mean decode time, 32.06 ms mean jitter buffer. 2560x1600 refinements
committed successfully (last id 57). During 408 zero-distance relative-input
packets over approximately 6.5 seconds, there were zero new refinement commits
in the measured active-input window and video advanced 6.017 seconds over six
seconds. Input ACK median after sustained activity was 0.3 ms, p95 0.5 ms.
Evidence: `.run/interaction-0.1.28-release.log`.

An added 100 ms application refinement ACK delay is not a WAN emulator. These
loopback tests do not establish real proxy input-to-visible latency or equality
to RDP. Continuous motion is intentionally low-resolution. No automatically
measured bandwidth adaptation is claimed. Physically audible playback on the
remote user's device requires user confirmation; audio regression measures the
decoded remote track while the local endpoint remains muted.
