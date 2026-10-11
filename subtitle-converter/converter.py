"""Periodically export embedded and sidecar subtitles through Subtitle Edit.

Only new SRT sidecars and the private state file are written. Source files and
existing destination files are never replaced. No external service is called.
"""

from __future__ import annotations

import argparse
from collections import Counter
import json
import logging
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tempfile
import threading
import time

LOG = logging.getLogger("subtitle-converter")
VIDEOS = {".mkv", ".mks", ".mp4", ".m4v", ".webm", ".mov", ".avi", ".ts", ".m2ts"}
SIDECARS = {".ass", ".ssa", ".vtt", ".ttml", ".smi", ".sami", ".sbv", ".sup", ".sub", ".idx"}
TIMING = re.compile(r"^\d{2,}:\d{2}:\d{2},\d{3} --> \d{2,}:\d{2}:\d{2},\d{3}(?: .*)?$")


def run_command(command, *, timeout, stop=None):
    """Bound runtime and captured output; stop OCR descendants with their parent."""
    with tempfile.TemporaryDirectory(prefix="subtitle-tool-") as temporary, \
            tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        environment = dict(os.environ, TMPDIR=temporary, TEMP=temporary, TMP=temporary)
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                                   start_new_session=os.name != "nt", env=environment)
        deadline = time.monotonic() + timeout
        try:
            while process.poll() is None:
                if (stop and stop.is_set()) or time.monotonic() >= deadline:
                    raise TimeoutError("Conversion timed out or was cancelled")
                try:
                    process.wait(timeout=0.2)
                except subprocess.TimeoutExpired:
                    pass
        finally:
            if process.poll() is None or process.returncode:
                if os.name == "nt":
                    taskkill = Path(os.environ["SystemRoot"]) / "System32" / "taskkill.exe"
                    subprocess.run([str(taskkill), "/PID", str(process.pid), "/T", "/F"],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=False)
                else:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                process.wait()
        stdout.seek(0)
        result = stdout.read(8 * 1024 * 1024 + 1)
        if len(result) > 8 * 1024 * 1024:
            raise ValueError("Tool output exceeded limit")
        if process.returncode:
            raise RuntimeError("Local conversion tool failed")
        return result


def language_map():
    """ISO mappings from Debian iso-codes; common aliases for native installs."""
    entries = {"en": "eng", "fr": "fra", "fre": "fra", "ar": "ara", "ja": "jpn",
               "de": "deu", "ger": "deu", "es": "spa", "zh": "zho", "chi": "zho"}
    try:
        table = json.loads(Path("/usr/share/iso-codes/json/iso_639-2.json").read_text())
        for item in table["639-2"]:
            canonical = item["alpha_3"]
            for key in (canonical, item.get("alpha_2"), item.get("bibliographic")):
                if key:
                    entries[key] = canonical
    except (OSError, ValueError, KeyError):
        pass
    return entries


def filename_language(path):
    parts = path.stem.lower().split(".")
    while parts and parts[-1] in {"forced", "hi", "sdh", "cc"}:
        parts.pop()
    return parts[-1] if len(parts) > 1 else "und"


def fingerprint(path: Path) -> list[list[int]]:
    """Track VobSub companions together, so changing either triggers a retry."""
    paths = [path]
    if path.suffix.lower() in {".sub", ".idx"}:
        companion = path.with_suffix(".idx" if path.suffix.lower() == ".sub" else ".sub")
        if companion.exists():
            if companion.is_symlink():
                raise ValueError("Symlink companion")
            paths.append(companion)
    return [[item.stat().st_size, item.stat().st_mtime_ns] for item in paths]


def candidates(roots: list[Path]):
    """Do not follow symlinked files or directories outside the selected roots."""
    seen = set()
    for root in roots:
        if not root.is_dir() or root.is_symlink():
            LOG.warning("A configured library directory is missing or is a symlink")
            continue
        resolved_root = root.resolve()
        for folder, directories, names in os.walk(root, followlinks=False):
            directories[:] = sorted(name for name in directories if not name.startswith(".") and not (Path(folder) / name).is_symlink())
            for name in sorted(names):
                path = Path(folder) / name
                suffix = path.suffix.lower()
                if suffix not in VIDEOS | SIDECARS or path.is_symlink():
                    continue
                # A VobSub pair is one conversion input, not two.
                if suffix == ".idx" and path.with_suffix(".sub").exists():
                    continue
                resolved = path.resolve()
                if not resolved.is_relative_to(resolved_root) or resolved in seen:
                    continue
                seen.add(resolved)
                yield resolved


def valid_srt(path: Path) -> bool:
    """Reject empty, truncated, or non-UTF-8 exports before publishing."""
    try:
        blocks = re.split(r"\n\s*\n", path.read_text(encoding="utf-8-sig").strip())
    except (OSError, UnicodeError):
        return False
    if not blocks or not blocks[0]:
        return False
    for block in blocks:
        lines = block.splitlines()
        if len(lines) < 3 or not lines[0].strip().isdigit() or not TIMING.fullmatch(lines[1].strip()):
            return False
        if not any(line.strip() for line in lines[2:]):
            return False
    return True


def publish(source: Path, destination: Path) -> bool:
    """Commit a complete file with atomic no-overwrite filesystem operations."""
    if os.name == "nt":
        # Windows rename fails if the destination exists.
        os.rename(source, destination)
    else:
        # Same-filesystem hard link fails if the destination exists.
        os.link(source, destination)
        source.unlink()
    return True


class Converter:
    def __init__(self, roots, state_path, *, min_age=120, timeout=7200, retry_interval=3600,
                 seconv="seconv", ffprobe="ffprobe", mkvmerge="mkvmerge", ocr_language="eng", dry_run=False,
                 stop=None):
        self.roots = [Path(root).absolute() for root in roots]
        self.state_path = Path(state_path)
        self.min_age = min_age
        self.timeout = timeout
        self.retry_interval = retry_interval
        self.seconv = seconv
        self.ffprobe = ffprobe
        self.mkvmerge = mkvmerge
        self.ocr_language = ocr_language
        self.dry_run = dry_run
        self.stop = stop or threading.Event()
        self.languages = language_map()
        self.state = {}
        try:
            loaded = json.loads(self.state_path.read_text(encoding="utf-8"))
            if isinstance(loaded, dict):
                self.state = loaded
        except FileNotFoundError:
            pass
        except (OSError, ValueError):
            LOG.warning("State could not be loaded; unchanged destinations will still be preserved")

    def run(self, command, timeout=None):
        return run_command(command, timeout=timeout or self.timeout, stop=self.stop)

    def ocr_for(self, language):
        if not isinstance(language, str) or not re.fullmatch(r"[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8})*", language):
            language = "und"
        primary = language.lower().split("-")[0]
        if primary in {"und", "", "unknown"}:
            LOG.warning("Image subtitle language is unknown; using configured OCR fallback")
            return self.ocr_language
        canonical = self.languages.get(primary, primary)
        return {"zho": "chi_tra" if any(part in language.lower() for part in ("hant", "tw", "hk")) else "chi_sim"}.get(canonical, canonical)

    def short_language(self, language):
        if not isinstance(language, str) or not re.fullmatch(r"[A-Za-z]{2,3}(?:-[A-Za-z0-9]{2,8})*", language):
            return "und"
        canonical = self.languages.get(language.lower(), language.lower())
        return next((code for code, mapped in self.languages.items() if len(code) == 2 and mapped == canonical), canonical)

    def tracks(self, path):
        """Use container-native IDs; stream ordinals are not Matroska track IDs."""
        if path.suffix.lower() in {".mkv", ".mks", ".webm"}:
            data = json.loads(self.run([self.mkvmerge, "-J", str(path)], 120))
            tracks = []
            for track in data.get("tracks", []):
                if track.get("type") == "subtitles":
                    properties = track["properties"]
                    tracks.append({"number": properties["number"],
                                   "language": properties.get("language_ietf") or properties.get("language", "und"),
                                   "forced": properties.get("forced_track", False),
                                   "hi": properties.get("hearing_impaired", False)})
            return tracks
        data = json.loads(self.run([self.ffprobe, "-v", "error", "-select_streams", "s",
                                   "-show_streams", "-of", "json", str(path)], 120))
        tracks = []
        for stream in data.get("streams", []):
            if path.suffix.lower() == ".avi":
                number = stream["index"]
            else:
                number = int(str(stream["id"]), 0)
            disposition = stream.get("disposition", {})
            tracks.append({"number": number, "language": stream.get("tags", {}).get("language", "und"),
                           "forced": bool(disposition.get("forced")), "hi": bool(disposition.get("hearing_impaired"))})
        return tracks

    def jobs(self, path, staging):
        if path.suffix.lower() in VIDEOS:
            tracks = self.tracks(path)
            counts = Counter((self.short_language(track["language"]), track["forced"], track["hi"]) for track in tracks)
            jobs = []
            for track in tracks:
                language = self.short_language(track["language"])
                stem = path.stem
                if counts[(language, track["forced"], track["hi"])] > 1 or language == "und":
                    stem += f".track{track['number']}"
                stem += f".{language}"
                if track["forced"]:
                    stem += ".forced"
                if track["hi"]:
                    stem += ".hi"
                jobs.append((path, stem + ".srt", track["number"], self.ocr_for(track["language"])))
            return jobs
        idx = path if path.suffix.lower() == ".idx" else path.with_suffix(".idx")
        if path.suffix.lower() in {".idx", ".sub"} and idx.exists():
            # VobSub's CLI loader has no track selection. Isolate each IDX
            # language and its timestamp/filepos block against the same payload.
            text = idx.read_text(encoding="utf-8-sig")
            sections = list(re.finditer(r"(?mi)^id:\s*([a-zA-Z-]+),\s*index:\s*\d+.*$", text))
            if not sections:
                raise ValueError("VobSub IDX has no language sections")
            payload = idx.with_suffix(".sub")
            if not payload.is_file() or payload.is_symlink():
                raise ValueError("VobSub payload is missing or is a symlink")
            staged_payload = staging / "vobsub.sub"
            shutil.copyfile(payload, staged_payload)
            header = re.sub(r"(?mi)^langidx:.*$", "langidx: 0", text[:sections[0].start()])
            jobs = []
            for index, section in enumerate(sections):
                language = section[1].lower()
                end = sections[index + 1].start() if index + 1 < len(sections) else len(text)
                body = text[section.start():end]
                body = re.sub(r"(?mi)^(id:.*index:)\s*\d+", r"\g<1> 0", body)
                staged_idx = staging / f"vobsub{index}.idx"
                # Each IDX needs a matching payload basename.
                shutil.copyfile(staged_payload, staged_idx.with_suffix(".sub"))
                staged_idx.write_text(header + body, encoding="utf-8")
                stem = path.stem
                if filename_language(path) != language:
                    stem += f".{self.short_language(language)}"
                if any(job[1] == stem + ".srt" for job in jobs):
                    stem += f".track{index}"
                jobs.append((staged_idx, stem + ".srt", None, self.ocr_for(language)))
            return jobs
        return [(path, path.stem + ".srt", None, self.ocr_for(filename_language(path)))]

    def save(self):
        self.state_path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=self.state_path.parent,
                                         prefix="state-", suffix=".tmp", delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(self.state, stream)
            stream.flush()
            os.fsync(stream.fileno())
        try:
            os.replace(temporary, self.state_path)
        finally:
            temporary.unlink(missing_ok=True)

    def convert(self, path):
        before = fingerprint(path)
        # Stage on the destination filesystem to permit an atomic no-overwrite
        # commit. The CLI may overwrite only its own disposable staging output.
        with tempfile.TemporaryDirectory(prefix=".srt-converter-", dir=path.parent) as folder:
            staging = Path(folder)
            jobs = self.jobs(path, staging)
            outputs = []
            destinations = []
            for source, name, number, language in jobs:
                destination = path.parent / name
                destinations.append(name)
                if destination.exists() or destination.is_symlink():
                    continue
                command = [self.seconv, str(source), "subrip", "--output-folder", str(staging),
                           "--output-filename", name, "--overwrite", "--encoding", "utf-8-no-bom",
                           "--ocr-engine", "tesseract", "--ocr-language", language, "--json"]
                if number is not None:
                    command.extend(["--track-number", str(number)])
                report = json.loads(self.run(command))
                output = staging / name
                if not report.get("success") or output.is_symlink() or not valid_srt(output):
                    raise ValueError("Invalid or missing SRT output")
                if report.get("warnings"):
                    LOG.warning("Subtitle Edit reported conversion warnings; check the resulting SRT")
                outputs.append(output)
            if fingerprint(path) != before or self.stop.is_set():
                raise RuntimeError("Source changed or conversion cancelled before publication")
            for output in outputs:
                destination = path.parent / output.name
                try:
                    publish(output, destination)
                except FileExistsError:
                    pass
            return destinations

    def scan(self):
        summary = {"converted": 0, "cached": 0, "pending": 0, "failed": 0, "planned": 0}
        for path in candidates(self.roots):
            if self.stop.is_set():
                break
            key = str(path)
            current = []
            try:
                current = fingerprint(path)
                now = time.time()
                if any(size == 0 or now - modified / 1e9 < self.min_age for size, modified in current):
                    summary["pending"] += 1
                    continue
                previous = self.state.get(key, {})
                if not isinstance(previous, dict):
                    previous = {}
                previous_outputs = previous.get("outputs", [])
                if not isinstance(previous_outputs, list) or any(not isinstance(name, str) or Path(name).name != name for name in previous_outputs):
                    previous = {}
                    previous_outputs = []
                if previous.get("fingerprint") == current:
                    if previous.get("ok") and all((path.parent / name).is_file() or (path.parent / name).is_symlink() for name in previous_outputs):
                        summary["cached"] += 1
                        continue
                    attempt = previous.get("attempt", 0)
                    if not isinstance(attempt, (int, float)):
                        attempt = 0
                    if not previous.get("ok") and now - attempt < self.retry_interval:
                        summary["pending"] += 1
                        continue
                if self.dry_run:
                    summary["planned"] += 1
                    LOG.info("Would inspect/convert: %s", path.name)
                    continue
                LOG.info("Processing: %s", path.name)
                outputs = self.convert(path)
                if fingerprint(path) != current:
                    raise RuntimeError("Source changed during conversion")
                self.state[key] = {"fingerprint": current, "outputs": outputs, "ok": True}
                self.save()
                summary["converted"] += 1
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                LOG.warning("Conversion failed for %s (%s); source and existing SRTs preserved", path.name, type(error).__name__)
                summary["failed"] += 1
                if not self.dry_run:
                    self.state[key] = {"fingerprint": current, "ok": False, "attempt": time.time()}
                    self.save()
        LOG.info("Scan: %s", ", ".join(f"{name}={count}" for name, count in summary.items()))
        return summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", action="append", help="Library directory; repeat for multiple roots")
    parser.add_argument("--once", action="store_true", help="Scan once then exit")
    parser.add_argument("--dry-run", action="store_true", help="List pending inputs; write nothing")
    parser.add_argument("--state", default=os.environ.get("CONVERTER_STATE", "/data/state.json"))
    args = parser.parse_args(argv)
    interval = int(os.environ.get("CONVERTER_SCAN_INTERVAL", "300"))
    min_age = int(os.environ.get("CONVERTER_MIN_AGE", "120"))
    timeout = int(os.environ.get("CONVERTER_TIMEOUT", "7200"))
    retry = int(os.environ.get("CONVERTER_RETRY_INTERVAL", "3600"))
    fallback_language = os.environ.get("CONVERTER_OCR_LANGUAGE", "eng")
    if not re.fullmatch(r"[a-zA-Z_]+(?:\+[a-zA-Z_]+)*", fallback_language):
        parser.error("OCR fallback must contain Tesseract language codes, optionally joined with +")
    if interval < 1 or min_age < 0 or timeout < 1 or retry < 1:
        parser.error("Intervals and timeout must be positive; minimum age must be nonnegative")
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    roots = args.root or ["/movies", "/movies-C", "/tv", "/tv-C"]
    seconv = shutil.which(os.environ.get("CONVERTER_SECONV", "seconv"))
    ffprobe = shutil.which(os.environ.get("CONVERTER_FFPROBE", "ffprobe"))
    mkvmerge = shutil.which(os.environ.get("CONVERTER_MKVMERGE", "mkvmerge"))
    if not args.dry_run and (not seconv or not ffprobe or not mkvmerge):
        parser.error("seconv, ffprobe and mkvmerge must be installed and available on PATH")
    stop = threading.Event()
    converter = Converter(roots, args.state, min_age=min_age, timeout=timeout, retry_interval=retry,
                          seconv=seconv, ffprobe=ffprobe, mkvmerge=mkvmerge, dry_run=args.dry_run, stop=stop,
                          ocr_language=fallback_language)
    signal.signal(signal.SIGTERM, lambda *_: stop.set())
    signal.signal(signal.SIGINT, lambda *_: stop.set())
    while not stop.is_set():
        summary = converter.scan()
        if args.once or args.dry_run:
            return 1 if summary["failed"] else 0
        stop.wait(interval)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
