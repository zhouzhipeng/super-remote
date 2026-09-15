# 0.1.32 sharp typing and automatic changes

Keyboard events no longer advance the mouse-interaction watermark or timer,
cancel PNG sends, or hide browser refinements. Native-resolution damage updates
continue during typing. Only mouse buttons/wheel request the existing temporary
video path; plain pointer motion remains sharp.

Outside that mouse-interaction window, all changed tiles retain the previous
sharp frame pending a lossless replacement. Automatic scrolling/animations do not
open holes onto low-resolution video. The 300 ms refresh gate is reduced to a
16 ms capture cadence; one in-flight update still bounds reliable backlog.

Browser fixture checks key down/up retain the sharp layer while input packets
are sent, and mouse clicks/wheel still hide it. Host unit test checks both key
transitions leave refinement scheduling/cancellation state untouched. Existing
pixel reconstruction, startup, cancellation and resize tests remain applicable.

This prioritizes sharpness over smoothness for large automatic changes. Capture,
PNG encoding and one-update ACK turnaround can limit keyboard-to-visible latency;
no RDP-equivalent or fixed latency guarantee is made. The initial baseline and
refinement failure fallback can still show video before sharp pixels are ready.
