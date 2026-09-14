# 0.1.24 desktop latency work

The 0.1.23 regional transport serialized capture, PNG encoding, transmission,
browser commit and the return ACK. It therefore could not exceed 10 updates/s
with a 100 ms ACK round trip, even before capture/encoding costs. Its 32 KiB
outstanding-data threshold counted unacknowledged SCTP bytes, imposing another
WAN throughput limit rather than merely bounding unsent bytes.

0.1.24 pipelines up to eight ordered updates, with byte and age bounds before
the next capture. The outstanding SCTP threshold is 512 KiB. Vertical scroll
reuse sends coordinates instead of PNG pixels only after exact byte comparison
of the proposed source and destination. PNG updates remain lossless. The browser
applies updates in sequence and uses the previous committed image as the source
of every copy operation, including overlapping scroll regions.

## Measurements on 2026-09-11

`web/tests/run-native-input-e2e.mjs`, isolated Rust Host/signaling, Chrome,
1512x950 actual primary desktop, additional 100 ms application commit-ACK delay,
32-second measurement interval:

| Implementation | Committed updates/s | Initial input median RTT |
| --- | ---: | ---: |
| Packaged 0.1.23 | 7.84375 | 0.3 ms |
| New pipeline | 32.0 | 0.3 ms |

New-pipeline five-second windows delivered 160–161 updates. Maximum
capture-plus-encode times were 33–39 ms and maximum commit RTT was 126–152 ms,
including the artificial 100 ms delay. H.264 fallback and independent input
sockets passed. These runs used the same machine and test procedure, not a
pixel-identical prerecorded desktop.

This isolates application ACK behavior. It is **not** a real WAN bandwidth/loss
emulator, RDP comparison, or input-to-visible latency measurement. The desktop
updates in this run were not a controlled scrolling workload, and the logged
copy count was zero. Scroll correctness is covered separately below; no real
scroll speedup is claimed from this particular runtime measurement.

## Correctness checks

- Deterministic 384x512 scroll fixture shifted 37 pixels: nine of twelve tiles
  reused from the previous image, three PNG damage tiles, reconstructed RGB
  bytes exactly equal to the target. Damage payload is less than half the full
  image payload.
- Browser fixture queues consecutive updates before decoding finishes, applies
  overlapping copy operations from the correct predecessor, and verifies pixels.
- Window tests bound unacknowledged update count and bytes, validate ordered
  ACKs, and release capacity on acknowledgement.
- Existing resize, partial-message, malformed-input, cleanup and DPI tests remain.
- The native integration test now probes both input channels again after the
  32-second interval to detect sustained-session regressions.

Run with `REMOTE_TILE_TEST=1 REMOTE_TILE_ACK_DELAY_MS=100` and the existing
browser/FFmpeg environment variables. `HOST_EXECUTABLE` and
`SIGNALING_EXECUTABLE` select the exact build. No production service is stopped,
no real keys/text are typed, and no desktop pixels are saved by this test.
