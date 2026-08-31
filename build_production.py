#!/usr/bin/env python3
"""Build and package the native Windows production distribution."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import time
import tomllib
import zipfile


ROOT = Path(__file__).resolve().parent
RUNTIME_BINARIES = (
    "remote-signaling.exe",
    "remote-host.exe",
    "remote-control-panel.exe",
)
MANIFEST_NAME = "production-manifest.json"


def run(command: list[str], *, env: dict[str, str] | None = None) -> None:
    print("+", subprocess.list2cmdline(command), flush=True)
    subprocess.run(command, cwd=ROOT, env=env, check=True)


def command_output(command: list[str]) -> str:
    return subprocess.check_output(
        command,
        cwd=ROOT,
        text=True,
        encoding="utf-8",
        errors="replace",
    ).strip()


def workspace_version() -> str:
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    version = workspace.get("workspace", {}).get("package", {}).get("version")
    if not isinstance(version, str) or not version:
        raise RuntimeError("Cargo.toml 中缺少 workspace.package.version")
    return version


def rust_host_triple() -> str:
    for line in command_output(["rustc", "-vV"]).splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ").strip()
    raise RuntimeError("rustc -vV 未返回 host triple")


def git_metadata() -> tuple[str, bool]:
    try:
        commit = command_output(["git", "rev-parse", "HEAD"])
        dirty = bool(command_output(["git", "status", "--porcelain", "--untracked-files=all"]))
        return commit, dirty
    except (OSError, subprocess.CalledProcessError):
        return "unknown", True


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def remove_exact(path: Path, parent: Path) -> None:
    resolved = path.resolve()
    parent_resolved = parent.resolve()
    if resolved == parent_resolved or parent_resolved not in resolved.parents:
        raise RuntimeError(f"拒绝删除输出目录之外的路径：{resolved}")
    if resolved.is_dir():
        shutil.rmtree(resolved)
    elif resolved.exists():
        resolved.unlink()


def verify_web_distribution(web_dist: Path) -> None:
    index = web_dist / "index.html"
    if not index.is_file():
        raise RuntimeError("Web 构建没有生成 web/dist/index.html")
    assets = web_dist / "assets"
    if not assets.is_dir() or not any(assets.iterdir()):
        raise RuntimeError("Web 构建没有生成静态资源")


def verify_binary(path: Path) -> None:
    if not path.is_file() or path.stat().st_size < 1024:
        raise RuntimeError(f"生产二进制不存在或异常：{path}")
    with path.open("rb") as binary:
        if binary.read(2) != b"MZ":
            raise RuntimeError(f"文件不是 Windows PE 二进制：{path}")


def package_readme(version: str) -> str:
    return f"""Super Remote {version} - Windows production package

Requirements:
- Windows 10 1809 or later
- Python 3.11 or later
- NVIDIA NVENC or AMD AMF H.264 hardware encoder
- Internet access on first launch if FFmpeg and the QR-code Python package are not cached

Start from PowerShell:

    python start_remote_desktop.py

The launcher requests administrator privileges, detects the primary display and hardware
encoder, then starts the bundled Signaling, Host and Control Panel binaries. Runtime data,
credentials, logs and the permanent QR code are written under .run; downloaded tools are
written under .tools. Neither directory is included in this production archive.
"""


def write_manifest(package_dir: Path, version: str, triple: str) -> dict[str, object]:
    commit, dirty = git_metadata()
    files: dict[str, dict[str, object]] = {}
    for path in sorted(item for item in package_dir.rglob("*") if item.is_file()):
        relative = path.relative_to(package_dir).as_posix()
        if relative == MANIFEST_NAME:
            continue
        files[relative] = {"size": path.stat().st_size, "sha256": sha256(path)}
    manifest: dict[str, object] = {
        "schema": 1,
        "product": "super-remote",
        "version": version,
        "rust_target": triple,
        "architecture": platform.machine(),
        "git_commit": commit,
        "source_dirty": dirty,
        "built_at_unix": int(time.time()),
        "files": files,
    }
    (package_dir / MANIFEST_NAME).write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    return manifest


def create_archive(package_dir: Path, archive_path: Path) -> None:
    with zipfile.ZipFile(
        archive_path,
        "w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=9,
    ) as archive:
        for path in sorted(item for item in package_dir.rglob("*") if item.is_file()):
            archive.write(path, (Path(package_dir.name) / path.relative_to(package_dir)).as_posix())


def verify_archive(package_dir: Path, archive_path: Path) -> None:
    expected = {
        (Path(package_dir.name) / path.relative_to(package_dir)).as_posix()
        for path in package_dir.rglob("*")
        if path.is_file()
    }
    with zipfile.ZipFile(archive_path) as archive:
        actual = {item.filename for item in archive.infolist() if not item.is_dir()}
        if actual != expected:
            missing = sorted(expected - actual)
            extra = sorted(actual - expected)
            raise RuntimeError(f"ZIP 文件集合不匹配；缺少={missing}，多出={extra}")
        corrupt = archive.testzip()
        if corrupt is not None:
            raise RuntimeError(f"ZIP CRC 校验失败：{corrupt}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="构建 Super Remote Windows 生产目录、ZIP 和 SHA-256 校验文件。"
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=ROOT / "artifacts",
        help="产物目录（默认：artifacts）",
    )
    parser.add_argument(
        "--target-dir",
        type=Path,
        default=ROOT / "target" / "production-package",
        help="独立 Cargo 构建缓存（默认：target/production-package）",
    )
    parser.add_argument("--skip-tests", action="store_true", help="跳过 Web 和 Rust 测试")
    parser.add_argument("--no-archive", action="store_true", help="只生成目录，不生成 ZIP")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if os.name != "nt":
        raise RuntimeError("生产二进制打包目前只支持 Windows")

    version = workspace_version()
    triple = rust_host_triple()
    if "windows" not in triple:
        raise RuntimeError(f"Rust host 不是 Windows target：{triple}")

    npm = shutil.which("npm.cmd") or shutil.which("npm")
    cargo = shutil.which("cargo")
    if not npm or not cargo:
        raise RuntimeError("构建需要 Node.js/npm 和 Rust/Cargo")

    run([npm, "--prefix", "web", "ci", "--no-audit", "--no-fund"])
    if not args.skip_tests:
        run([npm, "--prefix", "web", "test"])
        run([cargo, "test", "--workspace", "--locked"])
    run([npm, "--prefix", "web", "run", "build"])

    target_dir = args.target_dir.expanduser().resolve()
    build_env = os.environ.copy()
    build_env["CARGO_TARGET_DIR"] = str(target_dir)
    run(
        [
            cargo,
            "build",
            "--workspace",
            "--release",
            "--locked",
            "--bin",
            "remote-signaling",
            "--bin",
            "remote-host",
            "--bin",
            "remote-control-panel",
        ],
        env=build_env,
    )

    release_dir = target_dir / "release"
    for name in RUNTIME_BINARIES:
        verify_binary(release_dir / name)
    verify_web_distribution(ROOT / "web" / "dist")

    output_dir = args.output_dir.expanduser().resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    package_name = f"super-remote-{version}-{triple}"
    package_dir = output_dir / package_name
    archive_path = output_dir / f"{package_name}.zip"
    checksum_path = output_dir / f"{package_name}.zip.sha256"

    with tempfile.TemporaryDirectory(prefix=".super-remote-stage-", dir=output_dir) as temporary:
        stage = Path(temporary) / package_name
        (stage / "target" / "release").mkdir(parents=True)
        (stage / "web").mkdir(parents=True)
        (stage / "host").mkdir(parents=True)
        for name in RUNTIME_BINARIES:
            shutil.copy2(release_dir / name, stage / "target" / "release" / name)
        shutil.copytree(ROOT / "web" / "dist", stage / "web" / "dist")
        shutil.copy2(ROOT / "start_remote_desktop.py", stage / "start_remote_desktop.py")
        shutil.copy2(ROOT / "README.md", stage / "README.md")
        shutil.copy2(
            ROOT / "host" / "remote-host.example.toml",
            stage / "host" / "remote-host.example.toml",
        )
        (stage / "PACKAGE_README.txt").write_text(package_readme(version), encoding="utf-8")
        manifest = write_manifest(stage, version, triple)

        remove_exact(package_dir, output_dir)
        shutil.move(str(stage), package_dir)

    if args.no_archive:
        remove_exact(archive_path, output_dir)
        remove_exact(checksum_path, output_dir)
    else:
        remove_exact(archive_path, output_dir)
        remove_exact(checksum_path, output_dir)
        create_archive(package_dir, archive_path)
        verify_archive(package_dir, archive_path)
        checksum_path.write_text(f"{sha256(archive_path)}  {archive_path.name}\n", encoding="ascii")

    print(f"生产目录：{package_dir}")
    if not args.no_archive:
        print(f"生产压缩包：{archive_path}")
        print(f"SHA-256：{checksum_path}")
    print(f"清单文件数：{len(manifest['files'])}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.CalledProcessError, RuntimeError) as error:
        print(f"构建失败：{error}", flush=True)
        raise SystemExit(1)
