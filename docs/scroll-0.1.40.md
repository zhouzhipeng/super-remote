# 0.1.40 what else was making a scroll rough

0.1.39 paced multi-notch bursts. Three things were still working against a
scroll, only one of which is about wheel input at all.

## Refinement was taking the link from the video during the scroll

The largest of the three, and a regression introduced by 0.1.37. Refinement
keeps running while the user interacts so the delta baseline stays anchored, and
0.1.37 bounded one interaction update to 96 tiles — but never bounded the
*rate*. At a 200 ms cadence that is up to five 96-tile updates a second: on the
order of 20 Mbit/s of PNG, sharing a PeerConnection with a 6 Mbit/s video
stream, during exactly the moments the user is judging whether scrolling is
smooth. The justification at the time — "a scroll is mostly copy rectangles, so
it will be small" — is true of the copies and false of the newly exposed band,
which at a 200 ms gap is large.

The two constants are now a bandwidth budget rather than a size cap: 600 ms and
24 tiles, roughly 1 Mbit/s at a typical cost for a tile of text. The trade is
explicit — an older baseline costs one larger delta once the scroll ends, an
oversized one costs the video stream throughout it, and the scroll is what the
user is watching. 0.1.38's motion detector still backs off further from there.

## The encoder had two frames of VBV to fit a scroll into

`hybrid_encoding_args` set `bufsize` to two frames at the ceiling. A scroll is
full-frame motion over detailed text — the most expensive thing a desktop
encoder sees — and two frames of buffer forces the rate controller to fit it
inside twice the average frame, so quality collapses frame by frame for as long
as the scroll lasts. It is now six frames: enough to absorb the burst, still
bounding the queue it can build on a relay to about 100 ms. This is a
rate-control tuning change; it is the one item here that should be judged by
looking at a scroll rather than by reasoning about it.

## Wheel packets arrived in clumps and were injected in clumps

A relay delivers a stalled queue all at once. The Host injected each packet the
moment it arrived, so a clump became one jump — and Chromium is documented to
drop fine wheel events outright when they arrive in a burst. `WheelSmoother`
now spaces injection at the tick rate. Spacing engages only *above* that rate,
so ordinary per-frame scrolling is never held back and a scroll still starts on
the packet that begins it; the second packet of a clump waits 8 ms instead of
landing on top of the first.

## Still not subdivided by default

`wheel_step` remains 120, so a *single* notch is still injected whole and a
notched mouse still moves the remote screen in three-line steps. That is the
largest remaining lever and it is deliberately left to configuration: 40 — the
granularity Windows' own precision-touchpad stack sends to applications that
declare `highResolutionScrollingAware` — glides through a notch instead of
stepping it, but only in applications that accumulate high-resolution deltas.
Ones that compute `delta / WHEEL_DELTA` with integer division would not scroll
at all, and a scroll that does nothing is worse than a scroll that steps. It is
a judgement about the applications actually being driven, so it is the operator's
to make, not the default's.

## Coverage

The packet that begins a scroll is never held back; a second packet in the same
clump waits for the tick and is still owed; a third arriving a frame later
injects on arrival. The burst, reversal, precision-passthrough and i16-bound
cases from 0.1.39 continue to hold under a monotonic clock. Encoder arguments
assert the new buffer.
