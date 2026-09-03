#!/usr/bin/env python3
"""Build and run the LAN remote-desktop stack, then create a direct-access QR code."""

from __future__ import annotations

import ctypes
from ctypes import wintypes
import importlib
import base64
import hashlib
import hmac
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
import zipfile


ROOT = Path(__file__).resolve().parent
RUN_DIR = ROOT / ".run"
TOOLS_DIR = ROOT / ".tools"
FFMPEG_URL = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
DEFAULT_PORT = 8080
TURN_TCP_PORT = 3478
TURN_RELAY_MIN_PORT = 49160
TURN_RELAY_MAX_PORT = 49200
TURN_REALM = "super-remote"
DEVICE_ID = "local-windows-pc"
PERMANENT_EXPIRY = 253_402_300_799  # 9999-12-31T23:59:59Z
PRODUCTION_MANIFEST = ROOT / "production-manifest.json"
OBSOLETE_ICE_TCP_FIREWALL_RULE = "Super Remote Host ICE-TCP"


def is_process_elevated() -> bool:
    """Return true only for a fully elevated UAC token, not group membership."""
    token_query = 0x0008
    token_elevation = 20
    token = wintypes.HANDLE()
    advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetCurrentProcess.argtypes = []
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL
    advapi32.OpenProcessToken.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.HANDLE),
    ]
    advapi32.OpenProcessToken.restype = wintypes.BOOL
    advapi32.GetTokenInformation.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        wintypes.LPVOID,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD),
    ]
    advapi32.GetTokenInformation.restype = wintypes.BOOL
    if not advapi32.OpenProcessToken(
        kernel32.GetCurrentProcess(), token_query, ctypes.byref(token)
    ):
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        elevated = wintypes.DWORD()
        returned = wintypes.DWORD()
        if not advapi32.GetTokenInformation(
            token,
            token_elevation,
            ctypes.byref(elevated),
            ctypes.sizeof(elevated),
            ctypes.byref(returned),
        ):
            raise ctypes.WinError(ctypes.get_last_error())
        return elevated.value != 0
    finally:
        kernel32.CloseHandle(token)


def relaunch_as_administrator() -> None:
    """Relaunch this exact command through UAC so Host can control elevated apps."""
    shell32 = ctypes.WinDLL("shell32", use_last_error=True)
    shell32.ShellExecuteW.argtypes = [
        wintypes.HWND,
        wintypes.LPCWSTR,
        wintypes.LPCWSTR,
        wintypes.LPCWSTR,
        wintypes.LPCWSTR,
        ctypes.c_int,
    ]
    shell32.ShellExecuteW.restype = ctypes.c_void_p
    parameters = subprocess.list2cmdline([str(Path(__file__).resolve()), *sys.argv[1:]])
    result = shell32.ShellExecuteW(
        None,
        "runas",
        sys.executable,
        parameters,
        str(ROOT),
        1,
    )
    if not result or int(result) <= 32:
        raise ctypes.WinError(ctypes.get_last_error())


def process_image_path(process_id: int) -> Path | None:
    """Read a PID's image path without trusting status.json as an authority."""
    process_query_limited_information = 0x1000
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.QueryFullProcessImageNameW.argtypes = [
        wintypes.HANDLE,
        wintypes.DWORD,
        wintypes.LPWSTR,
        ctypes.POINTER(wintypes.DWORD),
    ]
    kernel32.QueryFullProcessImageNameW.restype = wintypes.BOOL
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL
    handle = kernel32.OpenProcess(process_query_limited_information, False, process_id)
    if not handle:
        return None
    try:
        buffer = ctypes.create_unicode_buffer(32_768)
        size = wintypes.DWORD(len(buffer))
        if not kernel32.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(size)):
            return None
        return Path(buffer.value).resolve()
    finally:
        kernel32.CloseHandle(handle)


def stop_existing_stack() -> None:
    """Stop only a previous stack whose executable paths match this project."""
    status_file = RUN_DIR / "status.json"
    if not status_file.exists():
        return
    try:
        previous = json.loads(status_file.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return
    expected = {
        "host_pid": (ROOT / "target" / "release" / "remote-host.exe").resolve(),
        "signaling_pid": (ROOT / "target" / "release" / "remote-signaling.exe").resolve(),
        "turn_pid": (ROOT / "target" / "release" / "remote-turn.exe").resolve(),
    }
    stopped_process_ids: list[int] = []
    for key, expected_path in expected.items():
        process_id = previous.get(key)
        if not isinstance(process_id, int) or process_image_path(process_id) != expected_path:
            continue
        print(f"正在停止旧实例：{expected_path.name} (PID {process_id})", flush=True)
        result = subprocess.run(
            ["taskkill.exe", "/PID", str(process_id), "/T", "/F"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if result.returncode != 0 and process_image_path(process_id) is not None:
            raise RuntimeError(f"无法停止旧进程 {process_id}")
        stopped_process_ids.append(process_id)
    if stopped_process_ids:
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if all(process_image_path(process_id) is None for process_id in stopped_process_ids):
                break
            time.sleep(0.1)


def run(command: list[str], *, env: dict[str, str] | None = None) -> None:
    print("+", subprocess.list2cmdline(command), flush=True)
    subprocess.run(command, cwd=ROOT, env=env, check=True)


def remove_obsolete_ice_tcp_firewall_rule(host_executable: Path) -> None:
    """Remove the ICE-TCP rule installed by the short-lived fallback release."""
    subprocess.run(
        [
            "netsh.exe",
            "advfirewall",
            "firewall",
            "delete",
            "rule",
            f"name={OBSOLETE_ICE_TCP_FIREWALL_RULE}",
            f"program={host_executable.resolve()}",
        ],
        cwd=ROOT,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )


def configure_turn_firewall(udp_port: int) -> None:
    """Allow only LAN clients to reach TURN control and its bounded relay range."""
    rules = (
        ("Super Remote TURN TCP", "TCP", str(TURN_TCP_PORT)),
        ("Super Remote TURN UDP", "UDP", str(udp_port)),
        (
            "Super Remote TURN Relay UDP",
            "UDP",
            f"{TURN_RELAY_MIN_PORT}-{TURN_RELAY_MAX_PORT}",
        ),
    )
    for name, protocol, local_port in rules:
        subprocess.run(
            [
                "netsh.exe",
                "advfirewall",
                "firewall",
                "delete",
                "rule",
                f"name={name}",
            ],
            cwd=ROOT,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        result = subprocess.run(
            [
                "netsh.exe",
                "advfirewall",
                "firewall",
                "add",
                "rule",
                f"name={name}",
                "dir=in",
                "action=allow",
                f"protocol={protocol}",
                f"localport={local_port}",
                "remoteip=localsubnet",
                "profile=any",
                "enable=yes",
            ],
            cwd=ROOT,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError(f"无法配置 TURN 防火墙规则：{name}")


def validate_production_package() -> None:
    required = [
        ROOT / "web" / "dist" / "index.html",
        *(ROOT / "target" / "release" / name for name in (
            "remote-signaling.exe",
            "remote-host.exe",
            "remote-control-panel.exe",
            "remote-turn.exe",
        )),
    ]
    missing = [str(path.relative_to(ROOT)) for path in required if not path.is_file()]
    if missing:
        raise RuntimeError(f"生产包不完整，缺少：{', '.join(missing)}")
    try:
        manifest = json.loads(PRODUCTION_MANIFEST.read_text(encoding="utf-8"))
        files = manifest["files"]
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise RuntimeError(f"生产包清单无效：{error}") from error
    if manifest.get("schema") != 1 or manifest.get("product") != "super-remote":
        raise RuntimeError("生产包清单的格式或产品名称无效")
    if not isinstance(files, dict) or not files:
        raise RuntimeError("生产包清单没有文件记录")
    root = ROOT.resolve()
    for relative, metadata in files.items():
        if not isinstance(relative, str) or not isinstance(metadata, dict):
            raise RuntimeError("生产包清单包含无效文件记录")
        path = (root / relative).resolve()
        if path == root or root not in path.parents:
            raise RuntimeError(f"生产包清单包含越界路径：{relative}")
        if not path.is_file():
            raise RuntimeError(f"生产包缺少清单文件：{relative}")
        expected_size = metadata.get("size")
        expected_hash = metadata.get("sha256")
        if path.stat().st_size != expected_size:
            raise RuntimeError(f"生产包文件大小校验失败：{relative}")
        digest = hashlib.sha256()
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                digest.update(chunk)
        if digest.hexdigest() != expected_hash:
            raise RuntimeError(f"生产包文件 SHA-256 校验失败：{relative}")


def find_ffmpeg() -> Path:
    compatible = list((TOOLS_DIR / "ffmpeg8").glob("**/bin/ffmpeg.exe"))
    if compatible:
        return compatible[0].resolve()
    system = shutil.which("ffmpeg")
    if system:
        return Path(system).resolve()
    matches = list((TOOLS_DIR / "ffmpeg").glob("**/bin/ffmpeg.exe"))
    if matches:
        return matches[0].resolve()
    TOOLS_DIR.mkdir(parents=True, exist_ok=True)
    archive = TOOLS_DIR / "ffmpeg.zip"
    print("首次运行：正在下载项目内 FFmpeg（用于 NVIDIA 硬件编码）…", flush=True)
    urllib.request.urlretrieve(FFMPEG_URL, archive)
    target = TOOLS_DIR / "ffmpeg"
    target.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive) as bundle:
        bundle.extractall(target)
    matches = list(target.glob("**/bin/ffmpeg.exe"))
    if not matches:
        raise RuntimeError("FFmpeg 下载完成，但找不到 ffmpeg.exe")
    return matches[0].resolve()


def ensure_qrcode() -> object:
    local_packages = TOOLS_DIR / "python"
    sys.path.insert(0, str(local_packages))
    try:
        import qrcode  # type: ignore
        return qrcode
    except ImportError:
        local_packages.mkdir(parents=True, exist_ok=True)
        run([sys.executable, "-m", "pip", "install", "--quiet", "--target", str(local_packages), "qrcode"])
        importlib.invalidate_caches()
        import qrcode  # type: ignore
        return qrcode


def lan_ip() -> str:
    command = [
        "powershell",
        "-NoProfile",
        "-Command",
        "Get-NetIPConfiguration | Where-Object { $_.IPv4DefaultGateway -ne $null -and "
        "$_.NetAdapter.Status -eq 'Up' } | ForEach-Object { $_.IPv4Address.IPAddress } | ConvertTo-Json -Compress",
    ]
    raw = subprocess.check_output(command, text=True, encoding="utf-8-sig").strip()
    candidates = json.loads(raw) if raw else []
    if isinstance(candidates, str):
        candidates = [candidates]
    for candidate in candidates:
        parts = candidate.split(".")
        if candidate.startswith("192.168.") or candidate.startswith("10."):
            return candidate
        if len(parts) == 4 and parts[0] == "172" and 16 <= int(parts[1]) <= 31:
            return candidate
    raise RuntimeError("找不到带默认网关的局域网 IPv4 地址")


def display_geometry() -> tuple[int, int, int, int]:
    user32 = ctypes.windll.user32
    try:
        user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))
    except (AttributeError, OSError):
        user32.SetProcessDPIAware()
    # EnumDisplaySettings reports the primary display's physical current mode
    # even if another imported Windows component fixed the process at a
    # DPI-virtualized awareness level before this call.
    class Point(ctypes.Structure):
        _fields_ = [("x", ctypes.c_long), ("y", ctypes.c_long)]

    class DevMode(ctypes.Structure):
        _fields_ = [
            ("dmDeviceName", ctypes.c_wchar * 32),
            ("dmSpecVersion", ctypes.c_ushort),
            ("dmDriverVersion", ctypes.c_ushort),
            ("dmSize", ctypes.c_ushort),
            ("dmDriverExtra", ctypes.c_ushort),
            ("dmFields", ctypes.c_ulong),
            ("dmPosition", Point),
            ("dmDisplayOrientation", ctypes.c_ulong),
            ("dmDisplayFixedOutput", ctypes.c_ulong),
            ("dmColor", ctypes.c_short),
            ("dmDuplex", ctypes.c_short),
            ("dmYResolution", ctypes.c_short),
            ("dmTTOption", ctypes.c_short),
            ("dmCollate", ctypes.c_short),
            ("dmFormName", ctypes.c_wchar * 32),
            ("dmLogPixels", ctypes.c_ushort),
            ("dmBitsPerPel", ctypes.c_ulong),
            ("dmPelsWidth", ctypes.c_ulong),
            ("dmPelsHeight", ctypes.c_ulong),
            ("dmDisplayFlags", ctypes.c_ulong),
            ("dmDisplayFrequency", ctypes.c_ulong),
            ("dmICMMethod", ctypes.c_ulong),
            ("dmICMIntent", ctypes.c_ulong),
            ("dmMediaType", ctypes.c_ulong),
            ("dmDitherType", ctypes.c_ulong),
            ("dmReserved1", ctypes.c_ulong),
            ("dmReserved2", ctypes.c_ulong),
            ("dmPanningWidth", ctypes.c_ulong),
            ("dmPanningHeight", ctypes.c_ulong),
        ]

    mode = DevMode()
    mode.dmSize = ctypes.sizeof(DevMode)
    if not user32.EnumDisplaySettingsW(None, -1, ctypes.byref(mode)):
        raise RuntimeError("无法读取主显示器的物理显示模式")
    primary_width = int(mode.dmPelsWidth)
    primary_height = int(mode.dmPelsHeight)
    logical_width = int(user32.GetSystemMetrics(0))
    logical_height = int(user32.GetSystemMetrics(1))
    scale_x = primary_width / logical_width
    scale_y = primary_height / logical_height
    return (
        primary_width,
        primary_height,
        round(int(user32.GetSystemMetrics(78)) * scale_x),
        round(int(user32.GetSystemMetrics(79)) * scale_y),
    )


def select_hardware_pipeline(ffmpeg: Path) -> tuple[str, str]:
    # Capability listing loads no capture or encoder device. The actual D3D11/NVENC
    # pipeline is intentionally opened only after a WebRTC peer is connected.
    encoders = subprocess.check_output(
        [str(ffmpeg), "-hide_banner", "-encoders"],
        cwd=ROOT,
        text=True,
        encoding="utf-8",
        errors="replace",
        stderr=subprocess.STDOUT,
    )
    if "h264_nvenc" in encoders:
        # Desktop Duplication keeps the full-resolution frame on D3D11 from
        # capture through scaling and NVENC. setpts in the Host assigns exact
        # 60 Hz timestamps to duplicated frames from otherwise-static desktops.
        return "h264_nvenc", "ddagrab"
    if "h264_amf" in encoders:
        return "h264_amf", "gdigrab"
    raise RuntimeError("NVENC 和 AMF 硬件 H.264 编码均不可用；按设计禁止 CPU 编码回退")


def json_request(url: str, *, body: dict[str, str] | None = None, token: str | None = None) -> object:
    data = json.dumps(body).encode() if body is not None else None
    headers = {"content-type": "application/json"}
    if token:
        headers["authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, data=data, headers=headers, method="POST" if data else "GET")
    with urllib.request.urlopen(request, timeout=3) as response:
        return json.loads(response.read())


def permanent_access_token(jwt_secret: str, subject: str) -> str:
    def encoded(value: object) -> str:
        raw = json.dumps(value, separators=(",", ":")).encode()
        return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()

    header = encoded({"typ": "JWT", "alg": "HS256"})
    claims = encoded(
        {
            "sub": subject,
            "role": "user",
            "exp": PERMANENT_EXPIRY,
            "iat": int(time.time()),
        }
    )
    payload = f"{header}.{claims}"
    signature = hmac.new(jwt_secret.encode(), payload.encode(), hashlib.sha256).digest()
    return f"{payload}.{base64.urlsafe_b64encode(signature).rstrip(b'=').decode()}"


def wait_for_health(base_url: str, process: subprocess.Popen[bytes], timeout: float = 20) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("信令服务启动失败，请查看 .run/signaling.log")
        try:
            with urllib.request.urlopen(f"{base_url}/api/healthz", timeout=1) as response:
                if response.read() == b"ok":
                    return
        except (OSError, urllib.error.URLError):
            time.sleep(0.25)
    raise RuntimeError("等待信令服务超时")


def wait_for_tcp_listener(
    address: str,
    port: int,
    process: subprocess.Popen[bytes],
    timeout: float = 10,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("TURN 服务启动失败，请查看 .run/turn.log")
        try:
            with socket.create_connection((address, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("等待 TURN/TCP 服务超时")


def wait_for_device(base_url: str, token: str, process: subprocess.Popen[bytes], timeout: float = 20) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("Host 启动失败，请查看 .run/host.log")
        try:
            devices = json_request(f"{base_url}/api/devices", token=token)
            if any(item.get("id") == DEVICE_ID and item.get("online") for item in devices):
                return
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(0.25)
    raise RuntimeError("Host 未能在信令服务中上线")


def main() -> int:
    if os.name != "nt":
        raise RuntimeError("这个启动脚本当前只支持 Windows")
    if not is_process_elevated():
        print("正在申请管理员权限（用于控制任务管理器等提升权限的窗口）…", flush=True)
        relaunch_as_administrator()
        return 0
    RUN_DIR.mkdir(parents=True, exist_ok=True)
    production_package = PRODUCTION_MANIFEST.is_file()
    if production_package:
        # Validate before stopping a healthy existing stack so a truncated or
        # corrupted package cannot strand the machine without a running Host.
        validate_production_package()
    stop_existing_stack()
    if "--stop" in sys.argv[1:]:
        print("远程桌面服务已停止。", flush=True)
        return 0
    # Establish per-monitor DPI awareness before importing qrcode/Pillow. Pillow
    # may otherwise lock Python to system-DPI awareness, making GetSystemMetrics
    # return the scaled logical desktop (for example 1512x950) instead of the
    # primary display's physical 2560x1600 pixels.
    primary_width, primary_height, desktop_width, desktop_height = display_geometry()
    if min(primary_width, primary_height, desktop_width, desktop_height) <= 0:
        raise RuntimeError("无法读取显示器尺寸")
    ffmpeg = find_ffmpeg()
    encoder, capture_mode = select_hardware_pipeline(ffmpeg)
    qrcode = ensure_qrcode()
    ip = lan_ip()
    # Capture the complete physical primary display. The Host uploads it to
    # D3D11 and scales to each browser's requested size on the GPU before NVENC, so the
    # browser receives the whole desktop instead of its top-left region.
    width, height = primary_width, primary_height

    secrets_file = RUN_DIR / "secrets.json"
    if secrets_file.exists():
        credentials = json.loads(secrets_file.read_text(encoding="utf-8"))
    else:
        credentials = {
            "jwt_secret": secrets.token_urlsafe(48),
            "device_token": secrets.token_urlsafe(36),
            "turn_secret": secrets.token_urlsafe(48),
            "username": "admin",
            "password": secrets.token_urlsafe(12),
            "port": DEFAULT_PORT,
        }
        secrets_file.write_text(json.dumps(credentials, indent=2), encoding="utf-8")
    credentials_changed = False
    for key, default in (
        ("username", "admin"),
        ("port", DEFAULT_PORT),
        ("turn_secret", secrets.token_urlsafe(48)),
    ):
        if key not in credentials:
            credentials[key] = default
            credentials_changed = True
    if credentials_changed:
        secrets_file.write_text(
            json.dumps(credentials, ensure_ascii=False, indent=2), encoding="utf-8"
        )
    username = credentials.get("username")
    password = credentials.get("password")
    port = credentials.get("port")
    turn_secret = credentials.get("turn_secret")
    if not isinstance(username, str) or not username.strip():
        raise RuntimeError("登录账号不能为空")
    if not isinstance(password, str) or len(password.encode("utf-8")) < 12:
        raise RuntimeError("登录密码必须至少包含 12 个 UTF-8 字节")
    if isinstance(port, bool) or not isinstance(port, int) or not 1 <= port <= 65535:
        raise RuntimeError("Web 服务端口必须是 1 到 65535 之间的整数")
    if not isinstance(turn_secret, str) or len(turn_secret.encode("utf-8")) < 32:
        raise RuntimeError("TURN 密钥必须至少包含 32 个 UTF-8 字节")

    if production_package:
        print("检测到生产二进制包，跳过 npm/Cargo 编译。", flush=True)
    else:
        run(["npm.cmd", "--prefix", "web", "install", "--no-audit", "--no-fund"])
        run(["npm.cmd", "--prefix", "web", "run", "build"])
        run([sys.executable, str(ROOT / "build_turn_server.py")])
        build_command = [
            "cargo",
            "build",
            "--release",
            "-p",
            "remote-signaling",
            "-p",
            "remote-host",
        ]
        # A running control panel keeps its executable locked on Windows. Panel-initiated
        # service restarts already run the installed panel binary, so rebuilding it here
        # would fail after the old Host/Signaling processes have been stopped.
        if "--from-control-panel" not in sys.argv[1:]:
            build_command.extend(["-p", "remote-control-panel"])
        run(build_command)

    base_url = f"http://{ip}:{port}"
    host_config = RUN_DIR / "remote-host.toml"
    escaped_ffmpeg = str(ffmpeg).replace("\\", "\\\\")
    host_config_text = (
        f'server_url = "{base_url}"\n'
        f'device_id = "{DEVICE_ID}"\n'
        'device_name = "这台 Windows 电脑"\n'
        f'device_token = "{credentials["device_token"]}"\n'
        f'width = {width}\nheight = {height}\nfps = 60\nbitrate = 20000000\nmonitor_index = 0\n'
        f'ffmpeg_path = "{escaped_ffmpeg}"\n'
        f'ffmpeg_encoder = "{encoder}"\n'
        f'ffmpeg_capture_mode = "{capture_mode}"\n'
        f'ffmpeg_capture_x = 0\nffmpeg_capture_y = 0\n'
        f'ffmpeg_capture_width = {primary_width}\n'
        f'ffmpeg_capture_height = {primary_height}\n'
        f'control_status_path = "{str(RUN_DIR / "host-state.json").replace("\\", "\\\\")}"\n'
        '\n[[ice_servers]]\n'
        'urls = ["stun:stun.l.google.com:19302"]\n'
    )
    # Parse before replacing the live configuration so launcher changes cannot
    # silently strand an already stopped stack with an invalid TOML file.
    tomllib.loads(host_config_text)
    host_config.write_text(host_config_text, encoding="utf-8")

    remove_obsolete_ice_tcp_firewall_rule(ROOT / "target" / "release" / "remote-host.exe")
    # Reuse the Web origin's numeric port for TURN/UDP. TCP 8089 remains the
    # HTTP/WebSocket service, while UDP 8089 is a separate socket and gives
    # macOS Chrome a relay endpoint on the exact LAN address/port it already
    # reached to load the application.
    turn_udp_port = port
    configure_turn_firewall(turn_udp_port)

    env = os.environ.copy()
    env.update(
        {
            "REMOTE_BIND": f"0.0.0.0:{port}",
            "REMOTE_WEB_DIST": str((ROOT / "web" / "dist").resolve()),
            "REMOTE_JWT_SECRET": credentials["jwt_secret"],
            "REMOTE_ADMIN_USER": username,
            "REMOTE_ADMIN_PASSWORD": password,
            "REMOTE_DEVICE_TOKEN": credentials["device_token"],
            # Chrome on macOS can silently fail TURN/TCP candidate gathering
            # while Safari on the same machine succeeds over UDP. Advertise
            # both transports, with UDP first, so relay-only Chromium still
            # has a working LAN path and TCP remains the fallback.
            "REMOTE_TURN_URLS": (
                f"turn:{ip}:{turn_udp_port}?transport=udp,"
                f"turn:{ip}:{TURN_TCP_PORT}?transport=tcp"
            ),
            "REMOTE_TURN_SECRET": turn_secret,
            "RUST_LOG": "remote_signaling=info,remote_host=info",
        }
    )
    turn_log = (RUN_DIR / "turn.log").open("ab", buffering=0)
    signaling_log = (RUN_DIR / "signaling.log").open("ab", buffering=0)
    host_log = (RUN_DIR / "host.log").open("ab", buffering=0)
    creation_flags = subprocess.CREATE_NEW_PROCESS_GROUP
    turn = subprocess.Popen(
        [
            str(ROOT / "target" / "release" / "remote-turn.exe"),
            "--public-ip",
            ip,
            "--realm",
            TURN_REALM,
            "--tcp-port",
            str(TURN_TCP_PORT),
            "--udp-port",
            str(turn_udp_port),
            "--min-port",
            str(TURN_RELAY_MIN_PORT),
            "--max-port",
            str(TURN_RELAY_MAX_PORT),
        ],
        cwd=ROOT,
        env=env,
        stdout=turn_log,
        stderr=subprocess.STDOUT,
        creationflags=creation_flags,
    )
    signaling: subprocess.Popen[bytes] | None = None
    host: subprocess.Popen[bytes] | None = None
    try:
        wait_for_tcp_listener(ip, TURN_TCP_PORT, turn)
        signaling = subprocess.Popen(
            [str(ROOT / "target" / "release" / "remote-signaling.exe")],
            cwd=ROOT,
            env=env,
            stdout=signaling_log,
            stderr=subprocess.STDOUT,
            creationflags=creation_flags,
        )
        wait_for_health(base_url, signaling)
        access_token = permanent_access_token(credentials["jwt_secret"], username)
        host = subprocess.Popen(
            [str(ROOT / "target" / "release" / "remote-host.exe"), str(host_config)],
            cwd=ROOT,
            env=env,
            stdout=host_log,
            stderr=subprocess.STDOUT,
            creationflags=creation_flags,
        )
        wait_for_device(base_url, access_token, host)
        direct_url = (
            f"{base_url}/?v={int(time.time())}"
            f"#token={access_token}&device={DEVICE_ID}"
        )
        qr_path = RUN_DIR / "remote-desktop-qr.png"
        qrcode.make(direct_url).save(qr_path)
        status = {
            "url": base_url,
            "direct_url": direct_url,
            "qr": str(qr_path),
            "username": username,
            "password": password,
            "port": port,
            "signaling_pid": signaling.pid,
            "host_pid": host.pid,
            "turn_pid": turn.pid,
            "turn_url": f"turn:{ip}:{turn_udp_port}?transport=udp",
            "turn_urls": [
                f"turn:{ip}:{turn_udp_port}?transport=udp",
                f"turn:{ip}:{TURN_TCP_PORT}?transport=tcp",
            ],
            "turn_relay_ports": f"{TURN_RELAY_MIN_PORT}-{TURN_RELAY_MAX_PORT}/udp",
            "launcher_pid": os.getpid(),
            "desktop": f"{desktop_width}x{desktop_height}",
            "primary_display": f"{primary_width}x{primary_height}",
            "stream": f"{width}x{height}",
            "encoder": encoder,
            "capture_mode": capture_mode,
            "elevated": True,
            "python_executable": str(Path(sys.executable).resolve()),
        }
        (RUN_DIR / "status.json").write_text(json.dumps(status, ensure_ascii=False, indent=2), encoding="utf-8")
        panel_executable = ROOT / "target" / "release" / "remote-control-panel.exe"
        subprocess.Popen(
            [str(panel_executable), str(ROOT)],
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS,
            close_fds=True,
        )
        print("\n远程桌面已启动", flush=True)
        print(f"手机访问：{base_url}", flush=True)
        print(f"二维码：{qr_path}", flush=True)
        print("二维码内含长期访问令牌；请勿转发。按 Ctrl+C 停止。", flush=True)
        while turn.poll() is None and signaling.poll() is None and host.poll() is None:
            time.sleep(1)
        exited = []
        for name, process in (
            ("remote-turn", turn),
            ("remote-signaling", signaling),
            ("remote-host", host),
        ):
            if process is None:
                continue
            return_code = process.poll()
            if return_code is not None:
                exited.append(f"{name}（退出码 {return_code}）")
        raise RuntimeError(
            f"{'、'.join(exited) or '子进程'}意外退出，请查看 .run 目录中的日志"
        )
    except KeyboardInterrupt:
        print("\n正在停止…", flush=True)
        return 0
    finally:
        for process in (host, signaling, turn):
            if process is not None and process.poll() is None:
                process.terminate()
        turn_log.close()
        signaling_log.close()
        host_log.close()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"启动失败：{error}", file=sys.stderr)
        raise SystemExit(1)
