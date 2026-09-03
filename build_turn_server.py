#!/usr/bin/env python3
"""Build the bundled Windows TURN/TCP relay with a pinned portable Go toolchain."""

from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import zipfile


ROOT = Path(__file__).resolve().parent
TOOLS_DIR = ROOT / ".tools"
GO_VERSION = "1.27.1"
GO_ARCHIVE = f"go{GO_VERSION}.windows-amd64.zip"
GO_URL = f"https://go.dev/dl/{GO_ARCHIVE}"
GO_SHA256 = "a3911b5e0e1b1053f25ed0675f4c1c6aad1e2bfcf253df2b9be4caabd2edd95d"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def ensure_go() -> Path:
    go_root = TOOLS_DIR / f"go-{GO_VERSION}"
    go_executable = go_root / "bin" / "go.exe"
    if go_executable.is_file():
        return go_executable

    TOOLS_DIR.mkdir(parents=True, exist_ok=True)
    archive = TOOLS_DIR / GO_ARCHIVE
    if not archive.is_file() or sha256(archive) != GO_SHA256:
        print(f"首次构建 TURN：正在下载 Go {GO_VERSION}…", flush=True)
        temporary_archive = TOOLS_DIR / f".{GO_ARCHIVE}.download"
        urllib.request.urlretrieve(GO_URL, temporary_archive)
        if sha256(temporary_archive) != GO_SHA256:
            temporary_archive.unlink(missing_ok=True)
            raise RuntimeError("Go 工具链下载文件的 SHA-256 校验失败")
        temporary_archive.replace(archive)

    with tempfile.TemporaryDirectory(prefix=".go-extract-", dir=TOOLS_DIR) as temporary:
        temporary_root = Path(temporary)
        with zipfile.ZipFile(archive) as bundle:
            bundle.extractall(temporary_root)
        extracted = temporary_root / "go"
        if not (extracted / "bin" / "go.exe").is_file():
            raise RuntimeError("Go 工具链压缩包缺少 go.exe")
        shutil.move(str(extracted), str(go_root))
    return go_executable


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="构建 Super Remote 的 TURN/TCP 中继。")
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "target" / "release" / "remote-turn.exe",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if os.name != "nt":
        raise RuntimeError("TURN 生产二进制目前只支持 Windows")
    go = ensure_go()
    output = args.output.expanduser().resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment["GOTOOLCHAIN"] = "local"
    environment["CGO_ENABLED"] = "0"
    subprocess.run(
        [
            str(go),
            "build",
            "-mod=mod",
            "-trimpath",
            "-ldflags=-s -w -H=windowsgui",
            "-o",
            str(output),
            ".",
        ],
        cwd=ROOT / "turn-server",
        env=environment,
        check=True,
    )
    if not output.is_file() or output.stat().st_size < 1024:
        raise RuntimeError("TURN 构建没有生成有效的 remote-turn.exe")
    print(f"TURN 二进制：{output}", flush=True)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.CalledProcessError, RuntimeError) as error:
        print(f"TURN 构建失败：{error}", file=sys.stderr, flush=True)
        raise SystemExit(1)
