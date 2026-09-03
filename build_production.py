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
import sys
import tempfile
import time
import tomllib
import zipfile


ROOT = Path(__file__).resolve().parent
RUNTIME_BINARIES = (
    "super-remote.exe",
    "remote-signaling.exe",
    "remote-host.exe",
    "remote-control-panel.exe",
    "remote-turn.exe",
)
FFMPEG_RUNTIME_FILES = (
    "ffmpeg.exe",
    "avcodec-62.dll",
    "avdevice-62.dll",
    "avfilter-11.dll",
    "avformat-62.dll",
    "avutil-60.dll",
    "swresample-6.dll",
    "swscale-9.dll",
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
- No separate Python, Node.js or FFmpeg installation

Start from the Start menu after installing, or run the portable executable directly:

    super-remote.exe

The native launcher requests administrator privileges, detects the primary display and LAN
address, then starts the bundled Signaling, Host, TURN and Control Panel binaries. The Web
application is embedded in remote-signaling.exe. Runtime data, credentials, logs and the
permanent QR code are written under C:\\ProgramData\\Super Remote. A bundled FFmpeg runtime
selects NVIDIA NVENC, AMD AMF, or software H.264 automatically.
"""


def find_ffmpeg_bundle() -> tuple[Path, Path, Path]:
    candidates = sorted((ROOT / ".tools" / "ffmpeg8").glob("**/bin/ffmpeg.exe"))
    for executable in candidates:
        binary_dir = executable.parent
        if all((binary_dir / name).is_file() for name in FFMPEG_RUNTIME_FILES):
            bundle_root = binary_dir.parent
            license_path = bundle_root / "LICENSE"
            readme_path = bundle_root / "README.txt"
            if license_path.is_file() and readme_path.is_file():
                return binary_dir, license_path, readme_path
    raise RuntimeError(
        "缺少可分发的 FFmpeg 8 shared 运行时；预期位于 .tools/ffmpeg8/**/bin"
    )


def find_iscc() -> str | None:
    direct = shutil.which("ISCC.exe") or shutil.which("iscc")
    if direct:
        return direct
    candidates = (
        Path(os.environ.get("LOCALAPPDATA", "")) / "Programs" / "Inno Setup 6" / "ISCC.exe",
        Path(os.environ.get("ProgramFiles(x86)", "")) / "Inno Setup 6" / "ISCC.exe",
        Path(os.environ.get("ProgramFiles", "")) / "Inno Setup 6" / "ISCC.exe",
    )
    return next((str(path) for path in candidates if path.is_file()), None)


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
    parser.add_argument("--no-installer", action="store_true", help="不生成 Windows Setup.exe")
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
    run([npm, "--prefix", "web", "run", "build"])
    if not args.skip_tests:
        run([npm, "--prefix", "web", "test"])
        run([cargo, "test", "--workspace", "--locked"])

    target_dir = args.target_dir.expanduser().resolve()
    build_env = os.environ.copy()
    build_env["CARGO_TARGET_DIR"] = str(target_dir)
    build_env["RUSTFLAGS"] = " ".join(
        filter(None, [build_env.get("RUSTFLAGS"), "-C target-feature=+crt-static"])
    )
    run(
        [
            cargo,
            "build",
            "--workspace",
            "--release",
            "--locked",
            "--bin",
            "super-remote",
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
    run(
        [
            sys.executable,
            str(ROOT / "build_turn_server.py"),
            "--output",
            str(release_dir / "remote-turn.exe"),
        ]
    )
    for name in RUNTIME_BINARIES:
        verify_binary(release_dir / name)
    verify_web_distribution(ROOT / "web" / "dist")
    ffmpeg_dir, ffmpeg_license, ffmpeg_readme = find_ffmpeg_bundle()

    output_dir = args.output_dir.expanduser().resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    package_name = f"super-remote-{version}-{triple}"
    package_dir = output_dir / package_name
    archive_path = output_dir / f"{package_name}.zip"
    checksum_path = output_dir / f"{package_name}.zip.sha256"
    installer_name = f"SuperRemote-{version}-windows-x64-setup.exe"
    installer_path = output_dir / installer_name
    installer_checksum_path = output_dir / f"{installer_name}.sha256"

    with tempfile.TemporaryDirectory(prefix=".super-remote-stage-", dir=output_dir) as temporary:
        stage = Path(temporary) / package_name
        (stage / "host").mkdir(parents=True)
        for name in RUNTIME_BINARIES:
            shutil.copy2(release_dir / name, stage / name)
        for name in FFMPEG_RUNTIME_FILES:
            shutil.copy2(ffmpeg_dir / name, stage / name)
        shutil.copy2(ffmpeg_license, stage / "FFMPEG-LICENSE.txt")
        shutil.copy2(ffmpeg_readme, stage / "FFMPEG-README.txt")
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

    if args.no_installer:
        remove_exact(installer_path, output_dir)
        remove_exact(installer_checksum_path, output_dir)
    else:
        iscc = find_iscc()
        if not iscc:
            raise RuntimeError(
                "生成 Setup.exe 需要 Inno Setup 6（ISCC.exe）；请先执行 "
                "winget install --id JRSoftware.InnoSetup -e"
            )
        remove_exact(installer_path, output_dir)
        remove_exact(installer_checksum_path, output_dir)
        run(
            [
                iscc,
                f"/DSourceDir={package_dir}",
                f"/DOutputDir={output_dir}",
                f"/DAppVersion={version}",
                f"/DOutputBaseFilename={Path(installer_name).stem}",
                str(ROOT / "installer" / "super-remote.iss"),
            ]
        )
        verify_binary(installer_path)
        installer_checksum_path.write_text(
            f"{sha256(installer_path)}  {installer_path.name}\n", encoding="ascii"
        )

    print(f"生产目录：{package_dir}")
    if not args.no_archive:
        print(f"生产压缩包：{archive_path}")
        print(f"SHA-256：{checksum_path}")
    if not args.no_installer:
        print(f"Windows 安装程序：{installer_path}")
        print(f"安装程序 SHA-256：{installer_checksum_path}")
    print(f"清单文件数：{len(manifest['files'])}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.CalledProcessError, RuntimeError) as error:
        print(f"构建失败：{error}", flush=True)
        raise SystemExit(1)
