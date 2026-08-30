#!/usr/bin/env python3
"""Build and run the LAN remote-desktop stack, then create a direct-access QR code."""

from __future__ import annotations

import ctypes
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
import urllib.error
import urllib.request
import zipfile


ROOT = Path(__file__).resolve().parent
RUN_DIR = ROOT / ".run"
TOOLS_DIR = ROOT / ".tools"
FFMPEG_URL = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
PORT = 8080
DEVICE_ID = "local-windows-pc"
PERMANENT_EXPIRY = 253_402_300_799  # 9999-12-31T23:59:59Z


def run(command: list[str], *, env: dict[str, str] | None = None) -> None:
    print("+", subprocess.list2cmdline(command), flush=True)
    subprocess.run(command, cwd=ROOT, env=env, check=True)


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
    return (
        int(user32.GetSystemMetrics(0)),
        int(user32.GetSystemMetrics(1)),
        int(user32.GetSystemMetrics(78)),
        int(user32.GetSystemMetrics(79)),
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
        # FFmpeg's ddagrab source can remain open without ever producing a frame
        # after a Windows display/session transition. gdigrab is the reliable
        # capture source on this host; encoding still stays on NVIDIA NVENC.
        return "h264_nvenc", "gdigrab"
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


def permanent_access_token(jwt_secret: str) -> str:
    def encoded(value: object) -> str:
        raw = json.dumps(value, separators=(",", ":")).encode()
        return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()

    header = encoded({"typ": "JWT", "alg": "HS256"})
    claims = encoded(
        {
            "sub": "admin",
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
    RUN_DIR.mkdir(parents=True, exist_ok=True)
    ffmpeg = find_ffmpeg()
    encoder, capture_mode = select_hardware_pipeline(ffmpeg)
    qrcode = ensure_qrcode()
    ip = lan_ip()
    primary_width, primary_height, desktop_width, desktop_height = display_geometry()
    if min(primary_width, primary_height, desktop_width, desktop_height) <= 0:
        raise RuntimeError("无法读取显示器尺寸")
    # gdigrab observes this scaled desktop as 1920x1200, covering the complete
    # primary display without a crop. Keeping that exact size avoids a blurry
    # resampling pass while still exceeding 1080p.
    width, height = 1920, 1200

    secrets_file = RUN_DIR / "secrets.json"
    if secrets_file.exists():
        credentials = json.loads(secrets_file.read_text(encoding="utf-8"))
    else:
        credentials = {
            "jwt_secret": secrets.token_urlsafe(48),
            "device_token": secrets.token_urlsafe(36),
            "password": secrets.token_urlsafe(12),
        }
        secrets_file.write_text(json.dumps(credentials, indent=2), encoding="utf-8")

    run(["npm.cmd", "--prefix", "web", "install", "--no-audit", "--no-fund"])
    run(["npm.cmd", "--prefix", "web", "run", "build"])
    run(["cargo", "build", "-p", "remote-signaling", "-p", "remote-host"])

    base_url = f"http://{ip}:{PORT}"
    host_config = RUN_DIR / "remote-host.toml"
    escaped_ffmpeg = str(ffmpeg).replace("\\", "\\\\")
    host_config.write_text(
        f'server_url = "{base_url}"\n'
        f'device_id = "{DEVICE_ID}"\n'
        'device_name = "这台 Windows 电脑"\n'
        f'device_token = "{credentials["device_token"]}"\n'
        f'width = {width}\nheight = {height}\nfps = 60\nbitrate = 14000000\nmonitor_index = 0\n'
        f'ffmpeg_path = "{escaped_ffmpeg}"\n'
        f'ffmpeg_encoder = "{encoder}"\n'
        f'ffmpeg_capture_mode = "{capture_mode}"\n'
        f'ffmpeg_capture_x = 0\nffmpeg_capture_y = 0\n'
        f'ffmpeg_capture_width = {width}\n'
        f'ffmpeg_capture_height = {height}\n',
        encoding="utf-8",
    )

    env = os.environ.copy()
    env.update(
        {
            "REMOTE_BIND": f"0.0.0.0:{PORT}",
            "REMOTE_WEB_DIST": str((ROOT / "web" / "dist").resolve()),
            "REMOTE_JWT_SECRET": credentials["jwt_secret"],
            "REMOTE_ADMIN_USER": "admin",
            "REMOTE_ADMIN_PASSWORD": credentials["password"],
            "REMOTE_DEVICE_TOKEN": credentials["device_token"],
            "RUST_LOG": "remote_signaling=info,remote_host=info",
        }
    )
    signaling_log = (RUN_DIR / "signaling.log").open("ab", buffering=0)
    host_log = (RUN_DIR / "host.log").open("ab", buffering=0)
    creation_flags = subprocess.CREATE_NEW_PROCESS_GROUP
    signaling = subprocess.Popen(
        [str(ROOT / "target" / "debug" / "remote-signaling.exe")],
        cwd=ROOT,
        env=env,
        stdout=signaling_log,
        stderr=subprocess.STDOUT,
        creationflags=creation_flags,
    )
    host: subprocess.Popen[bytes] | None = None
    try:
        wait_for_health(base_url, signaling)
        access_token = permanent_access_token(credentials["jwt_secret"])
        host = subprocess.Popen(
            [str(ROOT / "target" / "debug" / "remote-host.exe"), str(host_config)],
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
            "username": "admin",
            "password": credentials["password"],
            "signaling_pid": signaling.pid,
            "host_pid": host.pid,
            "launcher_pid": os.getpid(),
            "desktop": f"{desktop_width}x{desktop_height}",
            "primary_display": f"{primary_width}x{primary_height}",
            "stream": f"{width}x{height}",
            "encoder": encoder,
            "capture_mode": capture_mode,
        }
        (RUN_DIR / "status.json").write_text(json.dumps(status, ensure_ascii=False, indent=2), encoding="utf-8")
        print("\n远程桌面已启动", flush=True)
        print(f"手机访问：{base_url}", flush=True)
        print(f"二维码：{qr_path}", flush=True)
        print("二维码内含长期访问令牌；请勿转发。按 Ctrl+C 停止。", flush=True)
        while signaling.poll() is None and host.poll() is None:
            time.sleep(1)
        raise RuntimeError("子进程意外退出，请查看 .run 目录中的日志")
    except KeyboardInterrupt:
        print("\n正在停止…", flush=True)
        return 0
    finally:
        for process in (host, signaling):
            if process is not None and process.poll() is None:
                process.terminate()
        signaling_log.close()
        host_log.close()


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"启动失败：{error}", file=sys.stderr)
        raise SystemExit(1)
