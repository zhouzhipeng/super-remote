# 0.1.30 caret blink quality regression

Two defects caused idle quality flashing: any changed pixel exposed an entire
128x128 low-resolution tile, and each decoded refinement unconditionally hid
the full overlay before the host's later validation message.

The browser now commits new pixels only to its retained offscreen baseline,
keeping the last validated display visible until the next show. Resize and
actual interaction still invalidate the display. Input timestamp guards remain.
For at most 512 changed RGB pixels across equal-sized desktops, the Host keeps
existing sharp pixels until their lossless replacements arrive instead of
exposing low-resolution tiles. The small change is delayed, not discarded.
Larger animation damage still exposes live video in its affected regions.

Tests: a simulated 2x40 caret crosses two tiles, blinks on/off, and retains
sharp presentation in both phases; a larger change retains video masking.
Browser fixture verifies background commits neither hide nor modify the
validated display before show, masked copy baselines stay intact, and real
input events still hide the overlay (pointer movement alone does not).
