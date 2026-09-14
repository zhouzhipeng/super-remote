# 0.1.31 startup black-frame regression

Observed startup logs contained multiple FFmpeg starts (full resolution twice,
then hybrid) before the initial hybrid keyframe eight seconds later. Tile
negotiation used false both for unknown and for a completed video-only decision.
Every watch notification cancelled capture, including redundant values. PNG
refinements also began before the underlying video had presented any frame.

Changes: unresolved mode is now None; eligible sessions await negotiation before
starting capture (five-second fallback for legacy clients). Only a genuinely
different resolved mode cancels an existing capture. Refinement-v3 sends start,
then video-ready only after the browser video has nonzero dimensions and playable
frames. Host starts hybrid video after start but waits for video-ready before
sending PNGs. Timeout/channel failure falls back to video-only capture.

Unit tests cover unresolved mode, repeated notifications, and a real mode change.
Browser fixture asserts no premature video-ready and exactly one acknowledgement
across duplicate playing events. The live isolated test now asserts exactly one
FFmpeg start in hybrid mode before intentionally closing refinement for fallback.
These checks address startup sequencing; they cannot guarantee absence of every
black frame caused by display drivers, real desktop content, or network failure.

Resolution/quality transition follow-up: visible canvas dimensions remain unchanged
while new-size pixels decode. Validation resizes and draws atomically in one JS
task, avoiding an empty canvas between commit and show. A newly revealed sharp
layer fades in over 100 ms. Local input cancels animation and exposes current
video immediately. Browser tests check deferred resizing and transition creation.
