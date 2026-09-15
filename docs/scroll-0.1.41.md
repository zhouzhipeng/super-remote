# 0.1.41 a released gesture that would not stop

A regression from 0.1.40. After lifting the fingers, scrolling slowed to a crawl
and kept going for seconds.

`WheelSmoother` took its slice size from the most recent packet:

    self.slice_y = self.step.min(i32::from(delta_y).abs());

That value is the client's own granularity, and using it as a floor is correct —
precision input must not be subdivided below what arrived. Using it as the
*drain rate* is not. 0.1.40 added spacing at the tick rate, so packets arriving
faster than 125 Hz queue instead of injecting; macOS momentum then decays its
deltas toward a single unit while that backlog is still held. A few hundred
queued units draining one unit per 8 ms tick is several seconds of scrolling the
user cannot stop, which is exactly what it looked like.

The rate is now `max(finest, ceil(pending / DRAIN_TICKS))`, capped by `step` and
floored at one, so anything queued clears within eight ticks unless `step` is
deliberately pacing a genuinely long flick. It is fixed when input arrives
rather than recomputed per tick: recomputing from the shrinking remainder is a
decay curve, and a decay curve has no last tick — it would have turned seconds
of trickle into a shorter trickle rather than a stop.

Nothing else changes. One notch is still injected whole, a multi-notch flick is
still paced at `step`, precision input is still passed through untouched, and
net displacement is still exact.

Coverage: a momentum gesture whose packets arrive inside the spacing window and
whose deltas decay to a single unit must drain within `DRAIN_TICKS`. The test
fails against the previous drain behaviour, which is what makes it worth having.
