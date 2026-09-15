# Browser Remote Desktop

Browser-first remote desktop monorepo using Rust, WebRTC and a TypeScript client. The
implementation follows the supplied design's real-time-first rules: media uses WebRTC
(with an optional TURN/TCP bridge on the Web listener), H.264 is negotiated with constrained-baseline-compatible parameters,
pointer motion uses an unordered/unreliable channel, state transitions use a reliable
channel, coordinates are normalized after letterbox removal, and old input state is not
allowed to build an unbounded queue.

## Components

Windows clients negotiate `desktop-refinement-v3` alongside continuously running
H.264 (1280-pixel longest edge, 2 Mbps maximum). Only mouse button/wheel activity
requests the low-resolution interaction path. Pointer motion and keyboard events
do not hide, pause, or cancel native-resolution PNG/copy updates.

Without a mouse-triggered interaction window, automatic scrolling/animation and
typing retain the last sharp display until each new lossless update arrives.
Changed tiles no longer expose low-resolution video. Capture is polled at 16 ms;
there is one refinement in flight and no 300 ms idle-update throttle. This is not
a guaranteed 60 Hz update rate: PNG cost and transport/ACK RTT still apply.
Large automatic changes may update less smoothly on a slow link but stay sharp.
After mouse interaction, refinement resumes after a 250 ms idle window.

Startup determines the video mode before starting NVENC; PNGs wait for playable
video. Validated updates replace pixels atomically; quality recovery fades in
over 100 ms without delaying new mouse input. Fallback after refinement-channel
failure still uses video. Lossless means 8-bit GDI RGB, not HDR preservation.
Deploy the Host and Web together.

Validation: `npm test`, `node web/tests/run-desktop-tiles-e2e.mjs`, and Host tests
check exact damage/copy reconstruction, cancellation, stale snapshot rejection,
resize and cleanup. With the browser/FFmpeg test environment set,
`REMOTE_TILE_TEST=1 node web/tests/run-native-input-e2e.mjs` uses isolated services
and live capture without saving desktop pixels. It measures continuous video
and input ACKs, not input-to-visible latency. The optional
`REMOTE_TILE_ACK_DELAY_MS=100` delays only application refinement ACKs, not the
network or video stream. See `docs/latency-0.1.28.md` for limitations and results.

`web/tests/run-frp-tcp-e2e.mjs` runs the embedded Web UI, real Rust signaling and
bundled TURN behind an isolated TCP forwarder with a random external port. Its
synthetic H.264 Host never captures the desktop or injects input. The test forces
the Chrome client to use only the derived TCP relay and verifies decoded H.264,
data-channel round trips and rejection of unauthenticated TURN allocation.
Build Web and `remote-signaling` first, then run with `PLAYWRIGHT_PACKAGE`,
`CHROME_EXECUTABLE`, `TURN_EXECUTABLE` and `TEST_RELAY_IP` (the Host's physical LAN
IPv4) set. No installed service or existing browser session is stopped.

Legacy video-only NVENC uses constant QP 18 with spatial/temporal AQ disabled to keep static desktop
detail stable across periodic keyframes. H.264 remains lossy; this is not pixel-exact
lossless streaming. Bandwidth varies with screen activity;
the configured `bitrate` is still used by the developer AMF/software backends but is not a
bandwidth ceiling for the NVENC constant-quality path. The panel labels this mode
as “恒定画质”; the Web toolbar reports measured network bitrate. Retina clients
continue negotiating physical pixels up to the captured display resolution.

To verify static-image stability with the bundled FFmpeg and the real GPU, run
`python scripts/measure_ffmpeg_stability.py --ffmpeg "D:\Program Files\Super Remote\ffmpeg.exe" --production`.
This developer-only check loads the actual Rust encoder arguments, compares decoded
pixel hashes across keyframes on CPU/GPU inputs, and verifies a moving 60 FPS clip.
It writes test clips and measurements under `target/video-stability/` without
capturing the live desktop or connecting to an active remote session.

The control panel is opened automatically with the elevated Host. It shows the Host and
Signaling process state, browser connection state, active stream size/FPS/bitrate/encoder,
primary-display capture details and whether the capture pipeline is idle. Use its buttons
to start, stop or restart the stack, open the Web client, or view the current QR code. The
close button (or Alt+F4) first stops Host, Signaling, TURN and their FFmpeg descendants,
waits for the supervisor to exit, and then closes the panel. While stopping, repeated
close requests and new service operations are ignored; a failed stop keeps the panel
open with an error. Stop/Restart buttons continue to preserve the control panel.
Services enter a Windows kill-on-close Job Object at process creation, so killing the
supervisor also reclaims its service tree. The supervisor monitors the panel process
and stops the stack if the panel unexpectedly exits. FFmpeg startup probes and cancelled
capture sessions are cleaned up too. Cleanup validates executable paths and does not
kill other installations or unrelated programs with the same filename.

The
Web port, login account and password can be changed in the panel; saving preserves the
JWT/device secrets and restarts the services so the new address and login take effect. The
port must be between 1 and 65535. Passwords must contain at least 12 UTF-8 bytes and remain
masked in the panel.
The optional “Web 客户端连接后启用本机隐私黑屏” setting covers the complete Windows
virtual desktop, including every enabled secondary display, while a client is connected.
The overlay is excluded from Windows capture, so the remote picture and input continue
normally. The overlay opts out of DWM Peek fading and transitions. Window-show,
foreground and z-order events repair its topmost position without stealing focus;
the status timer also checks the order without repainting an unchanged overlay.
This remains a desktop-window overlay, not a physical display-disconnect or
secure-desktop security boundary: UAC, Ctrl+Alt+Delete, higher system window bands,
driver failures and asynchronous window-event races are not guaranteed to be hidden.
After the Web client disconnects, the overlay remains latched until a physical
keyboard or mouse attached to the Host generates input; injected `SendInput` events cannot
release it. “Web 客户端连接后静音主机声音”
mutes the Host's default playback endpoint without silencing the WebRTC loopback stream;
the endpoint remains muted after disconnect until the user explicitly unmutes it in Windows.
Both preferences are stored in `C:\ProgramData\Super Remote\control-settings.json` in an
installed build (or `.run/control-settings.json` in a developer checkout).

Input receives dedicated `THREAD_PRIORITY_HIGHEST` receiver threads (one fast mouse
channel and one reliable keyboard/button channel). Privacy keyboard/mouse hooks
have their own high-priority message pump, isolated from panel file I/O, DWM and
audio queries. Normal process priority is retained; no real-time scheduling is used.
Mouse backpressure stores only the newest unsent position; buttons and keys remain
ordered/reliable and do not wait for animation frames. Video requests the browser's
minimum safe playout buffer instead of a fixed additional 80 ms. These are latency
optimizations, not a zero-latency guarantee over a network or a slow target app.
  New clients negotiate two independent WebRTC PeerConnections: one carries video
  and audio, the other mouse/keyboard, clipboard and cursor metadata. ICE allocates
  separate UDP sockets or TURN allocations; fixed public port numbers are not
  required. Input uses an unordered, non-retransmitting movement channel and a
  reliable channel for keys, buttons and wheel deltas. A shared Host injection
  lock and position watermark reject moves older than an already applied click.
  The authenticated WebSocket control path remains the fallback if the input peer
  cannot connect or closes. Fallback is permanent for that session, releases held
  inputs, and rejects late RTC packets. Every control packet is bound to the exact
  authorized browser socket and session. Shared WAN/FRP congestion still affects
  both connections; separate transports do not provide bandwidth QoS.

  Local cursor rendering is negotiated explicitly. WGC, Desktop Duplication and
  GDI capture omit the cursor only for clients that request it; legacy clients
  retain the captured cursor. The browser renders a native cursor immediately,
  with Host shape/visibility updates at up to 31 Hz. Custom cursors include a PNG
  and hotspot (up to 128 px); unsupported shapes fall back to the arrow. Animated
  custom cursors use a static frame, and desktop-dependent XOR effects cannot be
  reproduced exactly in a PNG. Browser security prevents warping the user's OS
  pointer, so remote SetCursorPos operations do not relocate the local pointer.
  This is absolute-position desktop control, not a Pointer Lock gaming mode.
  Deploy Host, signaling and Web together to enable all new features.

  `web/tests/run-native-input-e2e.mjs` uses an isolated native Host and generated
  H.264 test pattern to verify separate ports, post-injection ACKs and WebSocket
  fallback while video stays connected. It injects only zero-distance relative
  moves. Set `PLAYWRIGHT_PACKAGE`, `CHROME_EXECUTABLE` and `FFMPEG_EXECUTABLE`;
  build `remote-host`, Web and then `remote-signaling` first. The FRP integration
  test also verifies two separate authenticated TCP relay allocations and local
  cursor metadata. Neither test replaces an installed service.

```powershell
npm --prefix web install
npm --prefix web run build
cargo test --workspace
```

To build a distributable Windows installer without replacing binaries used by a currently
running Host, install Inno Setup 6 and run:

```powershell
python build_production.py
```

The script uses an isolated Cargo target directory, builds the Web client before compiling it
into `remote-signaling.exe`, runs Web/Rust tests, performs locked release builds, validates
the PE binaries, and writes a standard `SuperRemote-<version>-windows-x64-setup.exe` plus a
portable ZIP and SHA-256 files under `artifacts/`. The installer registers an all-users Start
Menu shortcut and Windows uninstaller. Target computers need only Windows 10 1809 or later;
they do not need Python, Node.js, Rust, a separately installed FFmpeg, or loose Web assets.
The package includes the FFmpeg executable, its required shared libraries, and license files.
Use `--skip-tests`,
`--no-archive`, `--no-installer`, `--output-dir`, or `--target-dir` when needed.

Set non-development secrets before starting the server. No insecure fallback credentials
are compiled in.

```powershell
$env:REMOTE_JWT_SECRET = '<32-or-more-random-characters>'
$env:REMOTE_ADMIN_PASSWORD = '<12-or-more-characters>'
$env:REMOTE_DEVICE_TOKEN = '<24-or-more-random-characters>'
cargo run -p remote-signaling
```

Copy `host/remote-host.example.toml` to `remote-host.toml`, use the same device token,
and run:

```powershell
cargo run -p remote-host -- remote-host.toml
```

Open `http://127.0.0.1:8080`, sign in, select the online device and connect. For any
non-local deployment, terminate TLS at Nginx and use `https://`/`wss://` only.

The launcher's NVIDIA path follows Sunshine's low-latency encode architecture: capture the
complete physical primary display with DXGI Desktop Duplication → D3D11 GPU scaling/NV12 →
NVENC H.264 sized to the browser's physical video area → WebRTC. The output preserves the
display aspect ratio, never crops the source, and can stream the full physical primary-display
resolution to a high-DPI browser. Legacy configured bitrate scales with requested pixel count up to 20 Mbps;
hybrid clients use the bounded video rate described above.
It uses NVENC's low-latency P4 preset, AQ disabled, two
encoder surfaces, forced IDR frames and zero-latency tuning. DDA is sampled slightly above the
target rate, then surplus GPU frames are removed before NVENC to produce a stable 60 FPS encoded
stream without ever discarding dependent H.264 P-frames. A one-frame backpressured handoff
prevents a stale-frame queue while preserving every encoded H.264 reference frame.
Capture, D3D11 conversion, NVENC and WASAPI are
gated by WebRTC connection state: none of them starts while the Host is idle, and a normal
disconnect stops them immediately. The browser requests a small one-to-two-frame playout
buffer, and the Host uses exact 60 Hz timestamps to minimize frame jitter.
Pointer movement bypasses animation-frame batching and uses raw pointer updates when the
browser supports them. Click coordinates and button transitions are injected as one Win32
batch, while keyboard transitions remain reliable and ordered. The toolbar reports sampled
post-injection input RTT so input latency can be verified independently from video latency.
The browser keyboard mapper automatically remaps the left and right Command keys on Mac and
iPad clients to the matching remote Windows Control keys, so familiar Command shortcuts act
as their Ctrl equivalents on the Host.
When a WebRTC client connects on a multi-monitor Host, visible application windows whose
current monitor is not the primary display are consolidated onto the primary work area.
Normal windows keep their size unless they exceed the primary work area, relative placement
is preserved, and minimized or maximized windows retain their state. Desktop, taskbar,
transient shell and privacy-overlay windows are excluded.
Software encoder fallback is intentionally disabled; the application will not claim or
simulate hardware acceleration.
System playback audio uses event-driven WASAPI loopback capture and 48 kHz stereo Opus
in 20 ms WebRTC audio samples. Browsers require one user gesture before unmuting autoplay;
tap the `开启声音` button after connecting.
`h264_file` is optional and exists only to isolate WebRTC transport during integration tests.
Bidirectional text clipboard uses its own reliable WebRTC data channel. `Ctrl+V` replaces the
Host clipboard and injects the paste shortcut as one Host-side operation, while `Ctrl+C` reads
the result back to the browser. The toolbar clipboard panel provides explicit read, send,
send-and-paste and local-copy actions for mobile browsers and plain-HTTP LAN origins that deny
background clipboard writes. Copy followed by paste while the remote page keeps focus stays on
the Host clipboard, while leaving the page to copy locally resets the next paste to browser-to-Host
synchronization. Keyboard shortcuts use hidden browser-native copy and paste capture elements, so
the clipboard toolbar is only a fallback and is not part of the normal desktop workflow.
Multi-monitor, secure desktop and ICE restart remain the V2 items
identified by the design.

The production launcher probes bundled FFmpeg encoders before starting Host. For native
Media Foundation development diagnostics, run
`cargo run -p remote-host --example hardware_probe`; this path is not used by the installed
production capture pipeline.

## Production deployment

Copy `deploy/.env.example` to `deploy/.env`, replace every placeholder, update the two
domain names in `deploy/nginx/nginx.conf`, provide TLS certificates under both certificate
directories, then run `docker compose --env-file deploy/.env -f deploy/docker-compose.yml up -d`.
Do not expose the signaling service directly, enable coturn `no-auth`, or put permanent TURN
credentials in the browser bundle.


### 图片剪贴板

远程画面中的 Ctrl+C/Ctrl+V（Mac 上 Cmd+C/Cmd+V）支持图片：本机截图或复制的图片会分块发送到 Windows 主机，完整写入系统剪贴板后再粘贴；远程会话内复制的图片可以直接在主机粘贴。单张 PNG 上限为 16 MiB，解码像素内存上限为 64 MiB。

主机图片复制到本机通过现有的同机信令剪贴板桥接读取。HTTPS/localhost 使用浏览器 PNG 剪贴板 API，需要允许剪贴板访问；普通 HTTP 回退为富文本图片复制，能否在目标应用粘贴取决于浏览器和目标应用，不能保证等同于原生图片剪贴板。剪贴板面板的文本框仍用于手动同步文字。
