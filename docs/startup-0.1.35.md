# 0.1.35 first frame and first wheel

The initial stream was already 1280-edge H.264 followed by native-resolution
PNG refinement, but observed keyframe waits sometimes reached 6-8 seconds.
FFmpeg live input now uses explicit small probing limits (probesize 32,
analyzeduration 0, fpsprobesize 0), avoiding frame-rate analysis of a desktop
whose rate/format are already specified. No encoder restart is introduced.

Wheel events now send normalized pointer coordinates immediately before the
wheel delta on the same reliable ordered input transport. A newly connected
browser no longer depends on an earlier pointermove reaching the Host, and
an old unordered movement cannot undo the newer positioning watermark.
Position packets do not trigger a quality change; wheel packets still do.

Browser fixture explicitly sends a wheel event before any pointer movement and
checks the move-then-wheel order and center coordinates. Existing input/quality,
startup and pixel tests remain applicable. First-keyframe timing is recorded by
the isolated live test. It does not measure the user's actual WAN first-paint or
prove all startup delay comes from probing; cold GPU/display delays can remain.
