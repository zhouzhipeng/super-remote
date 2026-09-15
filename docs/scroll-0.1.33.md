# 0.1.33 scroll continuity

The 250 ms mouse idle gate allowed sharp PNG presentation between separated
wheel notches and during scroll tails. After 0.1.32 removed automatic damage
masking, that presentation could reuse the pre-scroll baseline, visibly jumping
back before a new PNG arrived.

Keep the video path for 900 ms after the latest wheel packet (mouse buttons
retain their 250 ms window). Both Host and browser enforce the wheel window.
A committed baseline records the mouse watermark at capture; it cannot be shown
after a newer mouse event until either a new update commits or a fresh capture
proves the old baseline is unchanged. Typing and pointer-only movement still do
not start an interaction window, and autonomous changes remain lossless outside
an active wheel/button window. Video resolution remains 1280 maximum edge.

Tests cover a 500 ms wheel gap, recovery after 950 ms, typing readiness, and
browser rejection of a sharp show while the wheel window is active. Pixel,
cancellation, key/mouse routing and startup regressions remain covered.
