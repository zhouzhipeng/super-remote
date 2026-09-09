"""Measure decoded static-desktop shimmer; uses only Python stdlib and FFmpeg.

This is a developer diagnostic, never an installed runtime dependency. Every
subprocess is windowless on Windows, and no live desktop/session is captured.
"""
from __future__ import annotations

import argparse
import json
import re
import shutil
import statistics
import subprocess
import tempfile
import time
from pathlib import Path


def run(ffmpeg: Path, args: list[str]) -> str:
    result = subprocess.run(
        [str(ffmpeg), "-hide_banner", "-loglevel", "error", *args],
        stdin=subprocess.DEVNULL, capture_output=True, text=True,
        encoding="utf-8", errors="replace",
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), timeout=90,
    )
    if result.returncode:
        raise RuntimeError(result.stderr[-4000:])
    return result.stdout


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ffmpeg", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("target/video-stability"))
    parser.add_argument("--production", action="store_true", help="Test exact Rust production arguments and assert stable decoded frames")
    parser.add_argument("--probe-qp", action="store_true", help="Compare complete YUV pixels at several fixed quality levels")
    parser.add_argument("--only", help="Run just one named scenario")
    options = parser.parse_args()
    options.output.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="static-", dir=options.output))
    source = "color=c=0x20242c:s=2560x1600:r=60,drawgrid=w=83:h=47:t=1:c=0x596273"
    for row in range(24):
        size = 13 + row % 4 * 3
        source += (
            f",drawtext=fontfile='C\\:/Windows/Fonts/consola.ttf':"
            f"text='Super Remote 2560x1600 Retina 0O1Il MW abcdef0123456789':"
            f"fontsize={size}:fontcolor=0xf1f3f5:x=40:y={30 + row * 60}"
        )
    common = ["-preset", "p4", "-tune", "ull", "-delay", "0", "-surfaces", "2",
              "-zerolatency", "1", "-forced-idr", "1", "-rc-lookahead", "0",
              "-temporal-aq", "0", "-strict_gop", "1", "-slices", "1"]
    scenarios = {
        "v015_cbr": ["-rc", "cbr", "-spatial-aq", "1", "-aq-strength", "8", "-b:v", "20M", "-maxrate", "20M", "-bufsize", "333333", "-g", "60"],
        "v016_cbr": ["-rc", "cbr", "-spatial-aq", "1", "-aq-strength", "8", "-b:v", "20M", "-maxrate", "20M", "-bufsize", "1333336", "-g", "120"],
        "vbr_cq18": ["-rc", "vbr", "-cq", "18", "-spatial-aq", "1", "-aq-strength", "8", "-b:v", "20M", "-maxrate", "20M", "-bufsize", "1333336", "-g", "120"],
        "vbr_cq18_no_aq": ["-rc", "vbr", "-cq", "18", "-spatial-aq", "0", "-b:v", "20M", "-maxrate", "20M", "-bufsize", "1333336", "-g", "120"],
        "constqp18": ["-rc", "constqp", "-qp", "18", "-spatial-aq", "0", "-g", "120"],
        "constqp18_long": ["-rc", "constqp", "-qp", "18", "-spatial-aq", "0", "-g", "600"],
        "cbr_intra_refresh": ["-rc", "cbr", "-spatial-aq", "1", "-aq-strength", "8", "-b:v", "20M", "-maxrate", "20M", "-bufsize", "1333336", "-g", "120", "-intra-refresh", "1"],
    }
    if options.production:
        cargo = shutil.which("cargo")
        if not cargo:
            raise RuntimeError("Rust is required to read the production encoder arguments")
        arguments = subprocess.run(
            [cargo, "run", "--quiet", "--locked", "-p", "remote-host", "--example", "ffmpeg_arguments"],
            cwd=Path(__file__).resolve().parents[1], capture_output=True, text=True, check=True,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), timeout=120,
        )
        settings = json.loads(arguments.stdout)
        common = []
        scenarios = {"production_cpu": settings, "production_gpu": settings, "production_mixed": settings, "production_motion": settings}
    elif options.probe_qp:
        scenarios = {f"qp{qp}": ["-rc", "constqp", "-qp", str(qp), "-spatial-aq", "0", "-g", "120"] for qp in (12, 14, 16, 18, 20)}
    if options.only:
        scenarios = {options.only: scenarios[options.only]}
    results = {}
    for name, settings in scenarios.items():
        clip = directory / f"{name}.mkv"
        try:
            gpu = name == "production_gpu"
            capture = "testsrc2=s=2560x1600:r=60" if name == "production_motion" else source
            if name == "production_mixed":
                capture = source + "[desktop];testsrc2=s=640x400:r=60[motion];[desktop][motion]overlay=x=1920:y=1200"
            device = ["-init_hw_device", "d3d11va=desktop", "-filter_hw_device", "desktop"] if gpu else []
            video_filter = "format=bgra,hwupload,scale_d3d11=width=2560:height=1600:format=nv12" if gpu else "format=nv12"
            encoded_at = time.perf_counter()
            run(options.ffmpeg, [*device, "-f", "lavfi", "-i", capture, "-frames:v", "360", "-an",
                                "-vf", video_filter, "-c:v", "h264_nvenc", *common, *settings,
                                "-profile:v", "baseline", "-bf", "0", str(clip)])
            encode_seconds = time.perf_counter() - encoded_at
            crop = "crop=2560:1120:0:0," if name == "production_mixed" else ""
            metadata = run(options.ffmpeg, ["-i", str(clip), "-an", "-vf",
                crop + "tblend=all_mode=difference,signalstats,metadata=print:file=-", "-f", "null", "-"])
            values = [float(v) for v in re.findall(r"lavfi.signalstats.YAVG=([0-9.e+-]+)", metadata)]
            steady = values[30:]
            result = {
                "decoded_frame_pairs": len(values), "steady_mean_luma_change": statistics.mean(steady),
                "steady_max_luma_change": max(steady), "bitrate_mbps": clip.stat().st_size * 8 / 6 / 1e6,
                "encode_seconds": encode_seconds, "encode_fps": 360 / encode_seconds,
                "largest_changes": sorted(enumerate(values, start=1), key=lambda p: p[1], reverse=True)[:10],
            }
            if options.production or options.probe_qp:
                filters = ["-vf", crop.rstrip(",")] if crop else []
                checksums = run(options.ffmpeg, ["-i", str(clip), "-an", *filters, "-f", "framemd5", "-"])
                hashes = [line.rsplit(",", 1)[1].strip() for line in checksums.splitlines() if line and not line.startswith("#")]
                result["decoded_frames"] = len(hashes)
                result["unique_decoded_frames"] = len(set(hashes))
                assert len(hashes) == 360, "frames were lost during encoding/decoding"
                for plane in ("U", "V"):
                    differences = [float(v) for v in re.findall("lavfi.signalstats." + plane + "AVG=([0-9.e+-]+)", metadata)]
                    result[f"max_{plane.lower()}_change"] = max(differences)
                result["pixels_identical"] = len(set(hashes)) == 1
            results[name] = result
        except RuntimeError as error:
            results[name] = {"error": str(error)}
        print(json.dumps({name: results[name]}), flush=True)
    # Machine-readable evidence accompanies the generated clips.
    (directory / "results.json").write_text(json.dumps(results, indent=2), encoding="utf-8")
    print(f"Evidence: {directory.resolve()}")
    if options.production and any("error" in value for value in results.values()):
        raise RuntimeError("production encoding validation failed")
    if options.production:
        for name, result in results.items():
            # H.264 4:2:0 is lossy; identical hashes are recorded, not promised.
            # Bound measured variation well below a single 8-bit code value:
            # old CBR peaks at >2 luma levels on this same static test pattern.
            if name != "production_motion" and (
                result["steady_max_luma_change"] > 0.01
                or result["max_u_change"] > 0.01 or result["max_v_change"] > 0.01
            ):
                raise RuntimeError(f"{name}: static-pixel variation exceeds 0.01/255 per plane")


if __name__ == "__main__":
    main()
