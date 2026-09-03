# Browser Remote Desktop

Browser-first remote desktop monorepo using Rust, WebRTC and a TypeScript client. The
implementation follows the supplied design's real-time-first rules: media bypasses the
signaling service, H.264 is negotiated with constrained-baseline-compatible parameters,
pointer motion uses an unordered/unreliable channel, state transitions use a reliable
channel, coordinates are normalized after letterbox removal, and old input state is not
allowed to build an unbounded queue.

## Components

- `protocol`: shared signaling types and the fixed-width binary input protocol.
- `signaling`: Axum HTTP/WebSocket service with user/device authentication, one-use
  WebSocket tickets, session authorization and short-lived coturn REST credentials.
- `web`: Vite/TypeScript browser client with device/session UI, WebRTC, fullscreen,
  keyboard/mouse forwarding and a `getStats()` debug overlay.
- `host`: Windows agent with webrtc-rs 0.20.x, trickle ICE, bundled FFmpeg
  Desktop Duplication/GDI capture, automatic NVENC/AMF/software H.264 selection,
  WASAPI loopback/Opus system audio, strict DataChannel validation
  and Win32 `SendInput` injection.
- `control-panel`: native Rust/Win32 Windows control panel for live service, client,
  capture and encoder status; start/stop/restart actions; Web/QR shortcuts; and an
  optional capture-excluded, local-input-released privacy screen.
- `launcher`: native Rust Windows launcher that handles elevation, credentials, display/LAN
  discovery, firewall rules, QR generation and service supervision without Python.
- `deploy`: production-oriented Nginx, signaling and authenticated coturn deployment.

## Local build

Requirements: Rust 1.94+, Node.js 24+, npm 11+, Windows 10 1809 or later for the Host.

For a one-command developer LAN test on Windows, run:

```powershell
python start_remote_desktop.py
```

The legacy developer script remains useful while changing capture pipelines. Production
builds instead use `super-remote.exe`: it performs the same orchestration natively and uses
the FFmpeg runtime included in the installer. The launcher probes the real encoder at startup,
then selects NVENC + Desktop Duplication, AMF + GDI, or a reduced-resolution software H.264
fallback. Target computers require no separate Python, Node.js or FFmpeg installation. Runtime
state and the long-lived direct-access QR code are written under `C:\ProgramData\Super Remote`.
The QR remains valid while `.run/secrets.json` is unchanged and contains a bearer token,
so treat it as a permanent password and do not share it. Windows Firewall must allow the
configured TCP Web port (8080 by default) from the local subnet, and the phone must be on
the same LAN.

The control panel is opened automatically with the elevated Host. It shows the Host and
Signaling process state, browser connection state, active stream size/FPS/bitrate/encoder,
primary-display capture details and whether the capture pipeline is idle. Use its buttons
to start, stop or restart the stack, open the Web client, or view the current QR code. The
Web port, login account and password can be changed in the panel; saving preserves the
JWT/device secrets and restarts the services so the new address and login take effect. The
port must be between 1 and 65535. Passwords must contain at least 12 UTF-8 bytes and remain
masked in the panel.
The optional “Web 客户端连接后启用本机隐私黑屏” setting covers the complete Windows
virtual desktop, including every enabled secondary display, while a client is connected.
The overlay is excluded from Windows capture, so the remote picture and input continue
normally. After the Web client disconnects, the overlay remains latched until a physical
keyboard or mouse attached to the Host generates input; injected `SendInput` events cannot
release it. “Web 客户端连接后静音主机声音”
mutes the Host's default playback endpoint without silencing the WebRTC loopback stream;
the endpoint remains muted after disconnect until the user explicitly unmutes it in Windows.
Both preferences are stored in `C:\ProgramData\Super Remote\control-settings.json` in an
installed build (or `.run/control-settings.json` in a developer checkout).

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
resolution to a high-DPI browser. Bitrate scales with the requested pixel count up to 20 Mbps,
which keeps 60 FPS ahead of the WebRTC sender instead of queuing oversized encoded frames.
It uses NVENC's quality-oriented low-latency P4 preset, spatial AQ, a one-frame CBR buffer, two
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
