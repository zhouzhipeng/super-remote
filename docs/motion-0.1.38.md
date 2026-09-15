# 0.1.38 video playback and the fade layer's stacking plane

Two follow-ups to 0.1.37.

## Windowed video played as a slideshow

A video playing in a window was not smooth, while the same video made fullscreen
was. The asymmetry is `can_restore_snapshot`: an already-shown sharp layer is
never re-validated, because `shown` short-circuits the pixel comparison. So

- windowed, with the sharp layer already up from a still moment: it stays up and
  is repainted at whatever rate capture → encode → transfer → ACK sustains, a
  few frames per second across a relay; and
- fullscreen, reached by *clicking* a control: the click retracts the layer, and
  a playing video can never satisfy the ≤512 changed pixel test that would bring
  it back, so H.264 keeps the picture — smooth, by accident.

Refinement also kept pushing whole-desktop PNG updates at the 16 ms cadence the
entire time, competing for the link with the video stream the user was actually
watching. Raising in-flight bytes to 192 KiB in 0.1.37 made it push harder.

A scene is now treated as animating after three consecutive updates that were
each slower than 150 ms (under 7 presentable updates per second), re-encoded at
least an eighth of the desktop's tiles, and were already out of date when the
next capture arrived. All three conditions are required, and each one excludes a
case that must keep its sharp incremental updates:

- *slow* alone would catch typing on a slow link — the size condition excludes
  a one or two tile keystroke;
- *large* alone would catch a window opening — the staleness condition excludes
  a single change followed by a still screen;
- *stale* alone would catch a caret — the cycle condition excludes anything this
  path can still deliver in time.

While animating, the sharp layer is retracted whole and refinement backs off to
1 s. It is retracted whole deliberately: 0.1.32 ruled out per-tile holes onto
low-resolution video because a patchwork of two resolutions looks broken, and
that judgement stands. A uniform handover to H.264 is the same thing that
already happens for mouse interaction. 0.1.32's "prioritizes sharpness over
smoothness for large automatic changes" is otherwise unchanged — this only
applies where the sharp path has demonstrably failed to be either.

Recovery needs no new mechanism: when motion stops, a capture matches the
baseline, the streak resets, and the existing stability confirmation shows the
sharp layer again. Refinement keeps committing at 1 s while animating precisely
so that baseline is still close enough to make recovery a small delta.

## The fade layer covered the toolbar hint

`.desktop-tiles-fade` shipped at `z-index: 2`, tying with `.connection-overlay`
and `.toolbar-corner-hint` and winning on DOM order, so every retraction covered
the top-left "控制条" hint for the length of the fade — one blink per mouse
click. It now shares `z-index: 1` with `.desktop-tiles`: both are desktop pixels
and belong under every overlay (2), the toolbar (3) and the clipboard panel (4).
The two are never visible simultaneously, so sharing a plane costs nothing. This
also fixes the pinned-mode case where an `inset: 0` layer covers the toolbar row
before the first `ResizeObserver` callback positions it.

Tests: the motion predicate is checked against a video window, a keystroke on a
slow link, a fast whole-desktop change and both sides of the size threshold. The
browser fixture asserts the fade layer shares a computed `z-index` with
`.desktop-tiles`. Existing pixel, cancellation, resize and input-routing
coverage is unchanged.

Not measured here. Whether a given video trips the detector depends on its size
on screen, host encode cost and relay RTT; `cycle_ms` and `animating` are on the
Host's commit log line.
