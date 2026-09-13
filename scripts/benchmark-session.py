#!/usr/bin/env python3
"""Portable fresh-process transcode-window benchmark; outputs one JSON report.

Example: python3 scripts/benchmark-session.py --engine target/release/chroma-engine \
    --media /media/movie.mkv --start-index 0 900 1700 --repeats 5 --count 2

Cache state is uncontrolled: this runner never evicts shared OS/media caches.
Install psutil for sampled worker RSS/CPU/I/O (never the Python runner's RSS).
Sampling can miss short peaks; OS/container high-water metrics remain authoritative.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import time
try:
    import psutil
except ImportError:
    psutil = None


def run_worker(command, timeout):
    """Drain output to files, sample only this worker, and kill/reap on deadline."""
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        worker = subprocess.Popen(command, stdout=stdout, stderr=stderr)
        start = time.monotonic()
        metrics = {"peakSampledRssBytes": None, "cpuSeconds": None, "readBytes": None,
                   "writeBytes": None, "sampleIntervalMs": 20, "sampleCount": 0,
                   "osPeakRssBytes": None}
        process = psutil.Process(worker.pid) if psutil else None
        timed_out = False
        try:
            while True:
                # POSIX wait4 returns this child's true high-water usage, even
                # when it exits between samples. Never use cumulative RUSAGE_CHILDREN.
                if hasattr(os, "wait4"):
                    pid, status, usage = os.wait4(worker.pid, os.WNOHANG)
                    if pid:
                        worker.returncode = os.waitstatus_to_exitcode(status)
                        metrics["osPeakRssBytes"] = usage.ru_maxrss * (1 if platform.system() == "Darwin" else 1024)
                        metrics["cpuSeconds"] = usage.ru_utime + usage.ru_stime
                        break
                elif worker.poll() is not None:
                    break
                if process:
                    try:
                        metrics["sampleCount"] += 1
                        metrics["peakSampledRssBytes"] = max(
                            metrics["peakSampledRssBytes"] or 0, process.memory_info().rss)
                        cpu = process.cpu_times()
                        metrics["cpuSeconds"] = cpu.user + cpu.system
                        if hasattr(process, "io_counters"):
                            io = process.io_counters()
                            metrics["readBytes"], metrics["writeBytes"] = io.read_bytes, io.write_bytes
                    except (psutil.Error, OSError):
                        pass
                if time.monotonic() - start >= timeout:
                    timed_out = True
                    worker.kill()
                    break
                time.sleep(0.02)
        finally:
            if worker.poll() is None:
                worker.kill()
            worker.wait()
        stdout.seek(0)
        stderr.seek(0)
        return worker.returncode, stdout.read().decode("utf-8", errors="replace"), \
            stderr.read().decode("utf-8", errors="replace"), metrics, timed_out


def positive(value):
    result = int(value)
    if result <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return result


def percentile(values, fraction):
    return sorted(values)[max(0, math.ceil(len(values) * fraction) - 1)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine", type=Path, required=True)
    parser.add_argument("--media", type=Path, required=True)
    parser.add_argument("--start-index", type=int, nargs="+", default=[0])
    parser.add_argument("--count", type=positive, default=2)
    parser.add_argument("--repeats", type=positive, default=3)
    parser.add_argument("--timeout", type=positive, default=120)
    parser.add_argument("--video-mode", choices=["copy", "h264"], default="copy")
    parser.add_argument("--metrics-required", action="store_true",
                        help="fail if optional psutil worker sampling is unavailable")
    parser.add_argument("--label", default="", help="hardware/build/cache conditions")
    parser.add_argument("--report", type=Path, help="write JSON to a new report file")
    args = parser.parse_args()
    if args.metrics_required and psutil is None:
        parser.error("install psutil to collect worker memory/CPU/I/O samples")
    if any(index < 0 for index in args.start_index):
        parser.error("start indices must be nonnegative")
    engine = args.engine.resolve(strict=True)
    media = args.media.resolve(strict=True)
    with engine.open("rb") as binary:
        digest_state = hashlib.sha256()
        for chunk in iter(lambda: binary.read(64 * 1024), b""):
            digest_state.update(chunk)
        digest = digest_state.hexdigest()
    report = {
        "schemaVersion": 1,
        "platform": platform.platform(), "architecture": platform.machine(),
        "engineSha256": digest, "mediaBytes": media.stat().st_size,
        "mode": args.video_mode, "cacheState": "uncontrolled",
        "measurement": "fresh-process contiguous-window wall time",
        "workerMetrics": "sampled-psutil" if psutil else "unavailable",
        "label": args.label, "resourcePolicy": os.environ.get("CHROMA_RESOURCE_POLICY", "default"),
        "runs": [],
    }
    failed = False
    for index in args.start_index:
        for repeat in range(args.repeats):
            with tempfile.TemporaryDirectory(prefix="chroma-benchmark-") as output:
                command = [str(engine), "transcode-fmp4-segments", str(media), output,
                           "--start-index", str(index), "--count", str(args.count),
                           "--video-mode", args.video_mode]
                start = time.monotonic()
                run = {"startIndex": index, "count": args.count, "repeat": repeat}
                try:
                    code, stdout, stderr, metrics, timed_out = run_worker(command, args.timeout)
                    run["exitCode"] = code
                    run["workerMetrics"] = metrics
                    if code or timed_out:
                        run["error"] = "worker exceeded deadline and was killed" if timed_out else stderr[-4096:]
                        failed = True
                    else:
                        engine_result = json.loads(stdout)
                        plan = engine_result.get("plan", {})
                        # Retain the measured windows, not thousands of unrelated
                        # movie chunks repeated in every benchmark sample.
                        chunk_plan = plan.get("chunks", {})
                        chunks = chunk_plan.get("chunks") if isinstance(chunk_plan, dict) else None
                        if isinstance(chunks, list):
                            plan["totalChunkCount"] = len(chunks)
                            chunk_plan["chunks"] = chunks[index:index + args.count]
                        run["engineResult"] = engine_result
                        run["outputBytes"] = sum(path.stat().st_size for path in Path(output).iterdir()
                                                 if path.is_file())
                except subprocess.TimeoutExpired:
                    run["error"] = "worker exceeded deadline and was killed"
                    failed = True
                except (OSError, ValueError) as error:
                    run["error"] = str(error)
                    failed = True
                run["elapsedSeconds"] = time.monotonic() - start
                report["runs"].append(run)
    for index in args.start_index:
        values = [run["elapsedSeconds"] for run in report["runs"]
                  if run["startIndex"] == index and "error" not in run]
        if values:
            report.setdefault("windowPercentiles", []).append({
                "startIndex": index, "successfulRuns": len(values),
                **{f"p{pct}Seconds": percentile(values, pct / 100) for pct in (50, 95, 99)},
            })
    result = json.dumps(report, indent=2)
    if args.report:
        with args.report.open("x", encoding="utf-8") as output:
            output.write(result + "\n")
        print(json.dumps({"report": str(args.report), "failed": failed,
                          "windowPercentiles": report.get("windowPercentiles", [])}, indent=2))
    else:
        print(result)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
