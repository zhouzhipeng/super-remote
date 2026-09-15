# 0.1.37 scroll quality and recovery

Three independent causes made scrolling blur the screen and then recover slowly.
All are addressed. The input path is untouched.

**The interaction stream was the whole picture, and it was 1280 px at 2 Mbps.**
Nothing switches resolution mid-session: H.264 runs at the interaction size for
the entire session and input only hides the sharp layer over it, so the step the
user sees is a layer swap onto a permanently downscaled stream.
`interaction_max_edge` (default 1920) and `interaction_bitrate` (default 6 Mbps)
are now configuration. `hybrid_encoding_args` no longer applies a second,
lower 4 Mbps ceiling that silently overrode whatever was configured.

**Refinement stopped entirely during interaction.** Capture, damage detection
and commits now continue throughout, so the delta baseline stays anchored to the
live screen. Previously it froze at the pre-scroll frame, the motion estimate
could not bridge the gap, and the first update after a scroll therefore
degenerated into a whole-desktop re-encode — precisely when the user was waiting
for it. While interacting, capture runs at 200 ms measured from the *end* of the
previous refinement, so a slow link throttles itself, and an update needing more
than 96 tiles is declined from damage detection alone, before any encoding: a
whole repaint waits for the window to close, a scroll band does not.

**The wheel window was 900 ms.** It is now 220 ms, buttons 200 ms, enforced on
both ends. Correctness no longer rests on the timer: a no-op capture may certify
the current input state only after the scene has held still for 60 ms, which is
what the long window was standing in for — a capture taken before the
application repainted an injected event is what allowed a pre-scroll frame to be
shown again. A commit still certifies its own watermark, so recovery after a
real scroll does not wait for that confirmation.

Supporting work: PNG encoding is split across scoped threads (byte-identical
output, covered by a test) instead of encoding a whole desktop on one core;
capture reuses its pixel buffer and fills RGB by indexed write rather than
per-pixel `extend_from_slice`; the per-tile refinement mask — computed every
tick but never transmitted since 0.1.32 — is replaced by an early-exit whole
buffer comparison; the vertical motion search covers ±1024 px so one slow
capture cannot push the real offset out of range; reliable bytes in flight go
from 16 KiB to 192 KiB, sent as 16 KiB messages. That last one matters most on a
relay: throughput was roughly window/RTT, so a multi-megabyte recovery update
was rate-limited to about 16 KiB per round trip.

Retracting the sharp layer hands its bitmap to a `.desktop-tiles-fade` layer
that fades out over 60 ms for a wheel and 120 ms otherwise. `.desktop-tiles`
still hides synchronously; nothing on the input path waits for the transition.

Presenting the sharp layer *during* an active scroll is deliberately not
implemented. It can only ever be capture + encode + transfer + decode + ACK
behind the live screen, so showing it while scrolling would trade blur for
visible input lag. Video remains the interaction path; this work shortens how
long it is used and narrows the quality gap while it is.

Tests: parallel and serial tile encoding produce identical bytes; an over-limit
update encodes nothing; a single changed pixel or a resize retires the baseline;
the interaction ceiling is configurable; the hybrid encoder honours its caller's
bitrate; the presentation window reopens on any fresh button/wheel packet. The
browser fixture checks the fade layer appears on retraction and is retired when
the sharp layer returns. Pixel reconstruction, cancellation, resize, startup and
input-routing coverage is unchanged.

Not measured here. Real recovery latency depends on desktop resolution, PNG
cost, host core count and relay RTT; no fixed figure is claimed. `commit_rtt_ms`
on the Host and `tileDecodeMs` / `tileReceiveToCommitMs` / `wheelRttMs` in the
browser report it. The defaults above assume a link that can carry 6 Mbps of
motion video; lower `interaction_bitrate` and `interaction_max_edge` together if
it cannot.
