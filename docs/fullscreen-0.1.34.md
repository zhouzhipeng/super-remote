# 0.1.34 fullscreen snapshot replay

A post-click PNG could have the current mouse watermark while still containing
an intermediate/windowed layout. The fresh capture comparison only determined
whether to encode another delta; it did not gate restoration over live video.

Restoring a hidden PNG overlay now requires matching dimensions and no more than
512 changed pixels versus the new capture (caret tolerance). Old fullscreen and
windowed layouts fail this comparison and stay offscreen; their decoded pixels
may still serve as an ordered delta baseline. Existing visible sharp updates
remain ordered and preserve the typing/automatic-change policy. Continuously
moving content after a click can keep using video until a sufficiently current
snapshot is available; avoiding stale-layout replay takes priority at that
cross-stream handoff.

Unit regression checks entering and leaving fullscreen, fresh baseline acceptance,
caret tolerance and preservation of the existing sharp-only automatic path.
This fixes the cross-stream presentation bug; it is not a claim of RTP packet
reordering or a measured synchronization guarantee for arbitrary video playback.
