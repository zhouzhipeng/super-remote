# 0.1.39 wheel pacing

## What was not the problem

The Host never quantized wheel input. `windows_input::inject` passes the
client's delta straight to `SendInput` as `mouseData` — `delta_y as i32 as u32`,
sign-extended, no rounding to `WHEEL_DELTA` — so a sub-notch delta from a
precision touchpad already reached Windows intact, and Chrome and other modern
applications already consumed it. There was no missing "high-resolution wheel
support" to add.

## What was

A browser coalesces wheel events to at most one per frame, summing whatever
arrived in between. A fast flick therefore reaches the Host as a *single* packet
carrying several notches, and `SendInput` applies all of it in one step: the
remote screen jumps where the local one would have travelled. The finer the
source device, the worse it gets, because more of the gesture lands in each
coalesced packet.

`WheelSmoother` splits a queued burst into steps of at most `wheel_step` units
(120 by default — one notch) injected every 8 ms, and never finer than the
client itself sent, so precision input that is already pixel-accurate passes
through untouched. The first step is injected on the receiving thread, so a
scroll still *starts* with no added latency; only the tail of a burst is paced.
Net displacement is preserved exactly, including a reversal mid-burst.

`wheel_step` is configuration because the trade-off is deployment-specific.
Lowering it to 40 or 20 glides through a single notch instead of stepping it —
the macOS feel — but only in applications that accumulate high-resolution
deltas. Browsers and modern applications do; some legacy Win32 applications
compute `delta / WHEEL_DELTA` with integer division and would not scroll at all.
The default never subdivides what arrived, so it cannot break anything that
worked before, and it still fixes the flick.

Pacing is driven by the session-long `remote-input-control` task, which outlives
any individual input channel. Wheel packets arrive on a different task, so a
`Notify` wakes the pacer when a burst leaves a remainder; an idle pointer costs
nothing and a queued burst cannot stall.

## Browser side

Three things made a continuous scroll the heaviest ordered traffic in the
session, on the same relay queue the scroll was waiting behind:

- every wheel event re-sent the pointer position, even when the pointer had not
  moved. It is now sent only when the position actually changed. Any pointer
  motion or button clears the record, so this only ever suppresses a repeat of a
  position the Host already has and nothing else could have moved since — the
  unreliable move path is never trusted to have arrived;
- every wheel event requested a latency echo, doubling the packets again.
  Pointer motion already samples one in sixteen; the wheel now samples one in
  eight;
- the sub-unit remainder was carried across a direction reversal, where it works
  against the new direction and delays its first unit. The accepted handling is
  to drop it, which is what the reversal now does.

## Coverage

Host: one notch stays one injection; a five-notch flick becomes five steps;
precision input is not subdivided; a smaller step subdivides a notch but still
not precision input; net displacement is exact across a reversal on both axes;
an extreme delta cannot produce a slice outside the protocol's i16. Browser: a
reversal delivers its first unit on the event that earns it, while a
same-direction remainder is still carried.

## Not addressed

Windows applies its own deceleration to scrolling, and Chrome animates a whole
notch. Pacing below one notch replaces that animation rather than adding to it;
stacking a third smoothing layer on top is what makes remote scrolling feel
floaty, so none is added here. Whether `wheel_step = 40` feels better than 120 is
a judgement about the applications actually being driven — measure it rather
than assume it.

Sources consulted for the Windows/Chrome wheel semantics above:
<https://learn.microsoft.com/en-us/windows/win32/w8cookbook/windows-precision-touchpad-devices>,
<https://github.com/electron/electron/issues/8960>,
<https://github.com/sumatrapdfreader/sumatrapdf/issues/3032>,
<https://bugzilla.mozilla.org/show_bug.cgi?id=1193202>.
