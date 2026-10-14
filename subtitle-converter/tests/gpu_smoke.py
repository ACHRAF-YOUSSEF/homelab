"""Hardware-only PaddleOCR integration smoke test for the optional GPU image.

Run inside the GPU image with /tests mounted read-only and no network. The
fixture media and service state live in a private temporary directory.
"""

from __future__ import annotations

import hashlib
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

from rust_smoke import Dashboard, PASSWORD, free_port, require_tool


BASE = "http://127.0.0.1"
JOB_TIMEOUT_SECONDS = 300


def _write_cues(path: Path, count: int) -> None:
    def timestamp(seconds):
        return f"{seconds // 3600:02d}:{seconds // 60 % 60:02d}:{seconds % 60:02d},000"
    with path.open("w", encoding="utf-8") as stream:
        for number in range(1, count + 1):
            start = number * 3
            stream.write(f"{number}\n{timestamp(start)} --> {timestamp(start + 1)}\nGPU OCR FIXTURE {number:04d}\n\n")


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _post_job(dashboard: Dashboard, sup: Path) -> str:
    response = dashboard.json(
        "POST",
        "/api/jobs",
        {"kind": "extract", "paths": [str(sup)], "languages": ["*"]},
    )
    job = response.get("job")
    if not isinstance(job, dict) or not isinstance(job.get("id"), str):
        raise AssertionError("service did not return a job identifier")
    return job["id"]


def _wait_job(dashboard: Dashboard, job_id: str, deadline: float, on_poll=None) -> dict:
    while time.monotonic() < deadline:
        status = dashboard.json("GET", "/api/status")
        job = next((item for item in status.get("history", []) if item.get("id") == job_id), None)
        if job is not None:
            if job.get("status") in {"completed", "failed", "cancelled"}:
                return job
        if on_poll is not None:
            on_poll()
        time.sleep(0.25)
    raise TimeoutError("GPU extraction job exceeded its deadline")


def _summary(job: dict) -> dict:
    summary = job.get("summary")
    if not isinstance(summary, dict):
        summary = {}
    if job.get("status") != "completed" or summary.get("failed", 0) != 0:
        diagnostics = {
            "status": job.get("status"),
            "error": job.get("error"),
            "summary": summary,
            "events": job.get("events"),
        }
        raise AssertionError(f"GPU extraction job did not complete cleanly: {json.dumps(diagnostics)}")
    return summary


def _memory_peak_bytes() -> int | None:
    try:
        value = Path("/sys/fs/cgroup/memory.peak").read_text(encoding="ascii").strip()
        return int(value)
    except (OSError, ValueError):
        return None


def _service_rss_high_water_bytes(pid: int) -> int | None:
    try:
        for line in Path(f"/proc/{pid}/status").read_text(encoding="ascii").splitlines():
            if line.startswith("VmHWM:"):
                return int(line.split()[1]) * 1024
    except (OSError, ValueError, IndexError):
        pass
    return None


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cues", type=int, choices=range(2, 501), default=2)
    args = parser.parse_args()
    require_tool("ffmpeg")
    require_tool("seconv")
    if not Path("/usr/local/bin/subforge").is_file():
        raise RuntimeError("service binary is missing")
    if os.environ.get("CONVERTER_ENABLE_GPU") != "true":
        raise RuntimeError("run this fixture only in the explicitly GPU-enabled image")

    with tempfile.TemporaryDirectory(prefix="subforge-gpu-smoke-") as temporary:
        workspace = Path(temporary)
        media = workspace / "media"
        data = workspace / "data"
        generated = workspace / "generated"
        media.mkdir()
        data.mkdir()
        generated.mkdir()

        source = workspace / "gpu-fixture.en.srt"
        _write_cues(source, args.cues)
        subprocess.run(
            [
                "seconv",
                str(source),
                "bluraysup",
                "--resolution:640x480",
                "--font-name:DejaVu Sans",
                "--font-size:32",
                "--overwrite",
                "--json",
                f"--output-folder:{generated}",
            ],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=45,
        )
        generated_sup = next(generated.glob("*.sup"), None)
        if generated_sup is None:
            raise AssertionError("Subtitle Edit did not generate the SUP fixture")
        sup = media / "OCR.en.sup"
        source_hash = _sha256(generated_sup)
        generated_sup.replace(sup)
        if _sha256(sup) != source_hash:
            raise AssertionError("fixture copy changed the SUP source")

        port = free_port()
        env = os.environ.copy()
        env.update(
            {
                "CONVERTER_UI_PASSWORD": PASSWORD,
                "CONVERTER_ALLOWED_HOSTS": "localhost,127.0.0.1",
                "CONVERTER_ENABLE_OCR": "true",
                "CONVERTER_ENABLE_GPU": "true",
                "CONVERTER_OCR_LANGUAGE": "eng",
                "CONVERTER_MIN_AGE": "0",
                "CONVERTER_TIMEOUT": str(JOB_TIMEOUT_SECONDS),
                "CONVERTER_RETRY_INTERVAL": "1",
            }
        )
        process = subprocess.Popen(
            [
                "/usr/local/bin/subforge",
                "--host",
                "127.0.0.1",
                "--port",
                str(port),
                "--data",
                str(data),
                "--root",
                str(media),
                "--no-auto-scan",
            ],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        start = time.monotonic()
        try:
            startup_deadline = time.monotonic() + 30
            while time.monotonic() < startup_deadline:
                if process.poll() is not None:
                    raise RuntimeError("service exited during startup")
                try:
                    with urllib.request.urlopen(f"{BASE}:{port}/health", timeout=0.5) as response:
                        if response.status == 200:
                            break
                except (OSError, urllib.error.URLError):
                    time.sleep(0.1)
            else:
                raise TimeoutError("service health endpoint did not become ready")

            dashboard = Dashboard(port)
            before_get = dashboard.json("GET", "/api/workers")
            if before_get.get("edition") != "cuda" or dashboard.json("GET", "/api/system").get("edition") != "cuda":
                raise AssertionError("GPU fixture requires the compiled CUDA edition")
            before_verification = before_get.get("detected", {}).get("gpu_verification")
            # Re-reading worker status must be observational and must not launch
            # the costly CUDA probe.
            after_get = dashboard.json("GET", "/api/workers")
            if after_get.get("detected", {}).get("gpu_verification") != before_verification:
                raise AssertionError("GET /api/workers changed GPU verification state")
            if after_get.get("detected", {}).get("gpu_health") is not None:
                raise AssertionError("GET /api/workers unexpectedly performed a CUDA health probe")
            initial_allocation = after_get.get("allocation", {})
            if initial_allocation != {
                "cpu_workers": 1,
                "ocr_workers": 1,
                "gpu_workers": 0,
                "paused": False,
            }:
                raise AssertionError("service-generated worker defaults are incorrect")
            configuration = data / "workers.conf"
            if not configuration.is_file() or configuration.stat().st_mode & 0o777 != 0o600:
                raise AssertionError("service did not create a private workers.conf")

            dashboard.json(
                "PUT",
                "/api/workers",
                {"cpu_workers": 1, "ocr_workers": 0, "gpu_workers": 1, "paused": False},
            )
            gpu_started = False

            def observe_gpu_worker() -> None:
                nonlocal gpu_started
                current = dashboard.json("GET", "/api/workers")
                active = current.get("active", {})
                if active.get("gpu_workers", 0) > 0 or any(
                    worker.get("resource") == "gpu" for worker in current.get("workers", [])
                ):
                    gpu_started = True

            first_id = _post_job(dashboard, sup)
            first_job = _wait_job(
                dashboard,
                first_id,
                time.monotonic() + JOB_TIMEOUT_SECONDS,
                observe_gpu_worker,
            )
            first_summary = _summary(first_job)
            output = media / "OCR.en.srt"
            if not output.is_file():
                raise AssertionError("GPU OCR did not produce the expected SRT")
            output_text = output.read_text(encoding="utf-8-sig").casefold()
            if "gpu" not in output_text or "fixture" not in output_text:
                raise AssertionError("GPU OCR output did not contain the fixture words")
            if _sha256(sup) != source_hash:
                raise AssertionError("OCR changed its SUP source")

            worker_status = dashboard.json("GET", "/api/workers")
            verified = worker_status.get("detected", {})
            health = verified.get("gpu_health")
            if verified.get("gpu_verification") != "verified" or not isinstance(health, dict):
                raise AssertionError("GPU worker completed without recording a verified CUDA probe")
            if health.get("cuda") is not True or health.get("device_count") != 1:
                raise AssertionError("recorded GPU health did not confirm exactly one CUDA device")
            if not gpu_started:
                raise AssertionError("GPU resource assignment was not observed during extraction")

            output_hash = _sha256(output)
            second_id = _post_job(dashboard, sup)
            second_job = _wait_job(dashboard, second_id, time.monotonic() + 30)
            second_summary = _summary(second_job)
            if second_summary.get("cached", 0) < 1 and second_summary.get("already_ready", 0) < 1:
                raise AssertionError("repeat extraction did not use the cached OCR result")
            if _sha256(output) != output_hash:
                raise AssertionError("cached extraction changed the existing SRT")

            elapsed = round(time.monotonic() - start, 2)
            metrics = {
                "ok": True,
                "edition": "cuda",
                "fixture_cues": args.cues,
                "first_job_converted": first_summary.get("converted", 0),
                "repeat_job_cached": second_summary.get("cached", 0),
                "active_gpu_observed": gpu_started,
                "cuda_verified": True,
                "source_unchanged": True,
                "memory_peak_bytes": _memory_peak_bytes(),
                "service_rss_high_water_bytes": _service_rss_high_water_bytes(process.pid),
                "elapsed_seconds": elapsed,
            }
            print(json.dumps(metrics, separators=(",", ":")))
        finally:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=5)


if __name__ == "__main__":
    main()
