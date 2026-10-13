"""Black-box text subtitle and MKV extraction smoke test for the Rust service image."""

from __future__ import annotations

import base64
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


PASSWORD = "fixture-password-long-enough"
BASE = "http://127.0.0.1"


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class Dashboard:
    def __init__(self, port: int) -> None:
        self.base = f"{BASE}:{port}"
        self.auth = "Basic " + base64.b64encode(f"admin:{PASSWORD}".encode()).decode()
        self.csrf = self.json("GET", "/api/session")["csrf_token"]

    def json(self, method: str, path: str, body: dict | None = None) -> dict:
        data = None if body is None else json.dumps(body).encode()
        headers = {"Authorization": self.auth}
        if data is not None:
            headers["Content-Type"] = "application/json"
            headers["X-CSRF-Token"] = self.csrf
        request = urllib.request.Request(self.base + path, data=data, headers=headers, method=method)
        with urllib.request.urlopen(request, timeout=3) as response:
            return json.loads(response.read())


def require_tool(name: str) -> str:
    path = shutil.which(name)
    if path is None:
        raise RuntimeError(f"required runtime tool is missing: {name}")
    return path


def write_srt(path: Path, text: str) -> None:
    path.write_text(f"1\n00:00:01,000 --> 00:00:02,000\n{text}\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--text-only",
        action="store_true",
        help="exercise text conversions with OCR disabled, suitable for a 512 MiB cgroup",
    )
    args = parser.parse_args()
    require_tool("ffmpeg")
    mkvmerge = require_tool("mkvmerge")
    require_tool("ffprobe")
    require_tool("seconv")
    with tempfile.TemporaryDirectory(prefix="subforge-smoke-") as temp:
        workspace = Path(temp)
        media = workspace / "media"
        data = workspace / "data"
        media.mkdir()
        data.mkdir()

        vtt = media / "Film.en.vtt"
        vtt.write_text(
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nVTT fixture line\n",
            encoding="utf-8",
        )
        existing = media / "Film.en.srt"
        existing.write_text("existing subtitle must survive\n", encoding="utf-8")
        vtt_to_convert = media / "Film.es.vtt"
        vtt_to_convert.write_text(
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nVTT fixture line\n",
            encoding="utf-8",
        )
        ass = media / "Film.fr.ass"
        ass.write_text(
            """[Script Info]
ScriptType: v4.00+
[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,1,2,0,2,10,10,10,1
[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,ASS fixture line
""",
            encoding="utf-8",
        )
        ttml = media / "Film.de.ttml"
        ttml.write_text(
            """<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml"><body><div>
<p begin="00:00:01.000" end="00:00:02.000">TTML fixture line</p>
</div></body></tt>
""",
            encoding="utf-8",
        )
        sbv = media / "Film.it.sbv"
        sbv.write_text("0:00:01.000,0:00:02.000\nSBV fixture line\n", encoding="utf-8")

        english_track = workspace / "track-en.srt"
        french_track = workspace / "track-fr.srt"
        write_srt(english_track, "Embedded English fixture")
        write_srt(french_track, "Embedded French fixture")
        mkv = media / "Tracks.mkv"
        subprocess.run(
            [
                mkvmerge,
                "-o",
                str(mkv),
                "--language",
                "0:eng",
                str(english_track),
                "--language",
                "0:fra",
                str(french_track),
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=20,
        )

        paths = [vtt, vtt_to_convert, ass, ttml, sbv, mkv]
        if not args.text_only:
            ocr_source = workspace / "ocr-source.en.srt"
            write_srt(ocr_source, "OCR fixture line")
            generated_ocr_dir = workspace / "ocr-generated"
            generated_ocr_dir.mkdir()
            subprocess.run(
                [
                    "seconv",
                    str(ocr_source),
                    "bluraysup",
                    "--resolution:640x480",
                    "--font-name:DejaVu Sans",
                    "--font-size:32",
                    "--overwrite",
                    "--json",
                    f"--output-folder:{generated_ocr_dir}",
                ],
                check=True,
                capture_output=True,
                text=True,
                timeout=30,
            )
            generated_sup = next(generated_ocr_dir.glob("*.sup"), None)
            if generated_sup is None:
                raise AssertionError("Subtitle Edit did not generate the OCR fixture")
            ocr_sup = media / "OCR.en.sup"
            shutil.copyfile(generated_sup, ocr_sup)
            paths.append(ocr_sup)

        port = free_port()
        env = os.environ.copy()
        env.update(
            {
                "CONVERTER_UI_PASSWORD": PASSWORD,
                "CONVERTER_ALLOWED_HOSTS": "localhost,127.0.0.1",
                "CONVERTER_ENABLE_OCR": "false" if args.text_only else "true",
                "CONVERTER_OCR_LANGUAGE": "eng",
                "CONVERTER_MIN_AGE": "0",
                "CONVERTER_TIMEOUT": "300",
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
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise RuntimeError(f"Subforge exited during startup: {process.stderr.read()}")
                try:
                    with urllib.request.urlopen(f"{BASE}:{port}/health", timeout=0.5) as response:
                        if response.status == 200:
                            break
                except (OSError, urllib.error.URLError):
                    time.sleep(0.05)
            else:
                raise TimeoutError("Subforge health endpoint did not become ready")

            dashboard = Dashboard(port)
            response = dashboard.json(
                "POST",
                "/api/jobs",
                {
                    "kind": "extract",
                    "paths": [str(path) for path in paths],
                    "languages": ["*"],
                },
            )
            job_id = response["job"]["id"]

            deadline = time.monotonic() + 90
            job = None
            while time.monotonic() < deadline:
                status = dashboard.json("GET", "/api/status")
                job = next((item for item in status["history"] if item["id"] == job_id), None)
                if job is not None:
                    break
                time.sleep(0.2)
            if job is None:
                raise TimeoutError("subtitle extraction job did not finish")
            if job["status"] != "completed" or job["summary"].get("failed", 0) != 0:
                raise AssertionError(f"unexpected job status: {job['status']}")

            assert existing.read_text(encoding="utf-8") == "existing subtitle must survive\n"
            for output, expected in (
                (media / "Film.es.srt", "VTT fixture line"),
                (media / "Film.fr.srt", "ASS fixture line"),
                (media / "Film.de.srt", "TTML fixture line"),
                (media / "Film.it.srt", "SBV fixture line"),
                (media / "Tracks.en.srt", "Embedded English fixture"),
                (media / "Tracks.fr.srt", "Embedded French fixture"),
            ):
                if not output.is_file():
                    raise AssertionError(f"missing expected SRT output: {output.name}")
                content = output.read_text(encoding="utf-8-sig")
                if expected not in content:
                    raise AssertionError(f"converted subtitle content missing from {output.name}")
            if not args.text_only:
                output = media / "OCR.en.srt"
                if not output.is_file() or "OCR fixture line" not in output.read_text(encoding="utf-8-sig"):
                    raise AssertionError("OCR extraction output is missing or incorrect")

            mode = "text-only conversions (including TTML and SBV)" if args.text_only else "text and OCR conversions"
            print(f"Rust HTTP service completed {mode}; existing SRT preserved")
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


if __name__ == "__main__":
    main()
