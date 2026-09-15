# 0.1.43 capturing the desktop the user actually sees

The Windows 11 taskbar rendered near black in the sharp layer while the video
layer showed it correctly. So did Start, notification flyouts, and any Mica or
WinUI chrome — the taskbar is just the one that is always on screen.

The two layers used different capture APIs that see different things. Video is
captured after DWM composition (WGC, or ddagrab's Desktop Duplication).
Refinement used GDI `BitBlt` on the screen DC, which reads the layer
*underneath* composition: an acrylic surface there is its transparent backing,
not the blurred result, and `SRCCOPY` discards alpha, so those regions arrived
as their near-black backing colour. This dated from the original hybrid
refinement and only became conspicuous as the sharp layer started being shown
more of the time.

Refinement now uses Desktop Duplication as well, so both layers capture the same
composited desktop.

The COM objects stay on one dedicated thread for the life of the session, and
the duplication persists across frames — it reports only what changed, so
recreating it per frame would both cost and lie. Only recycled buffers and
finished frames cross the channel, which is why the thread exists rather than
moving the objects through `spawn_blocking`.

Duplication answers "nothing new" rather than resending a frame, and the loop
still needs pixels. Those come from re-reading the staging texture that already
holds the last frame: keeping a second copy of the desktop would be a 24 MB
clone per frame at 4K. A frame whose `LastPresentTime` is zero is pointer
movement only, which the browser draws itself, and is treated the same way.
Acquire and release are paired even when a read fails, or the next acquire
returns `DXGI_ERROR_INVALID_CALL` for the rest of the session.

`DXGI_ERROR_ACCESS_LOST` — a mode change, a desktop switch, a full-screen
transition — recreates the duplication rather than the device. GDI remains as
the fallback: for a host where duplication cannot start at all, and per frame
while it is failing, with the session settling for GDI once it is clear the path
is not coming back rather than burning a capture and a log line every tick.
An HDR surface is rejected rather than converted; the lossless layer has never
claimed HDR.

The Windows-only half of this cannot be compiled by the usual `cargo check` on a
non-Windows machine, and the whole point of the change is code that must be
right the first time on hardware that is not here. It was verified by compiling
the exact `capture_gdi` / `DesktopDuplication` / `DesktopSource` sources for
`x86_64-pc-windows-msvc` in isolation — `windows` is pure bindings, so it
cross-compiles even though the full crate cannot. That check is clean, with no
warnings. It proves the code builds, not that a real GPU behaves; the
duplication paths themselves still need a run on the host.
