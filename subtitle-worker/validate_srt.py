"""Check SRT timing and readability; optionally compare two language tracks.

Usage: python validate_srt.py translated.en.srt --source transcript.ja.srt --video episode.mkv
"""

import argparse
import json
import os
import re
import subprocess
from pathlib import Path


STAMP = re.compile(r"^(\d{2,}):(\d{2}):(\d{2}),(\d{3})$")
ARROW = re.compile(r"^(.+?)\s+-->\s+(.+?)$")


def milliseconds(stamp):
    match = STAMP.fullmatch(stamp)
    if not match:
        raise ValueError(f"Invalid SRT timestamp: {stamp!r}")
    hours, minutes, seconds, millis = map(int, match.groups())
    if minutes > 59 or seconds > 59:
        raise ValueError(f"Invalid SRT timestamp: {stamp!r}")
    return ((hours * 60 + minutes) * 60 + seconds) * 1000 + millis


def parse_srt(path):
    content = Path(path).read_text(encoding="utf-8-sig").replace("\r\n", "\n").strip()
    cues, issues = [], []
    for number, block in enumerate(re.split(r"\n\s*\n", content), 1):
        lines = block.splitlines()
        if len(lines) < 3:
            issues.append(f"Block {number}: missing index, timestamp, or text")
            continue
        if lines[0].strip() != str(number):
            issues.append(f"Block {number}: unexpected cue index {lines[0]!r}")
        arrow = ARROW.fullmatch(lines[1].strip())
        if not arrow:
            issues.append(f"Block {number}: invalid timestamp line")
            continue
        try:
            start, end = (milliseconds(value) for value in arrow.groups())
        except ValueError as exc:
            issues.append(f"Block {number}: {exc}")
            continue
        text = "\n".join(lines[2:]).strip()
        if not text:
            issues.append(f"Cue {number}: empty text")
        if end <= start:
            issues.append(f"Cue {number}: end is not after start")
        if cues and start < cues[-1]["end"]:
            issues.append(f"Cue {number}: overlaps cue {cues[-1]['index']}")
        cues.append({"index": number, "start": start, "end": end, "text": text})
    return cues, issues


def video_duration_ms(path):
    result = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1:nokey=1", str(path)],
        capture_output=True, text=True, check=True, timeout=45,
    )
    return round(float(result.stdout.strip()) * 1000)


def inspect(target, source=None, video=None, max_cps=20):
    cues, issues = parse_srt(target)
    notes = []
    duration = video_duration_ms(video) if video else None
    for cue in cues:
        index, length = cue["index"], cue["end"] - cue["start"]
        lines = cue["text"].splitlines()
        if duration is not None and cue["end"] > duration + 500:
            issues.append(f"Cue {index}: ends after the video")
        if len(lines) > 2:
            notes.append(f"Cue {index}: more than 2 text lines")
        if any(len(line) > 42 for line in lines):
            notes.append(f"Cue {index}: line exceeds 42 characters")
        if length < 800:
            notes.append(f"Cue {index}: shown for under 0.8 seconds")
        if length > 7000:
            notes.append(f"Cue {index}: shown for over 7 seconds")
        if length > 0 and len(cue["text"].replace("\n", "")) * 1000 / length > max_cps:
            notes.append(f"Cue {index}: exceeds {max_cps} characters/second")
        if cue["text"].rstrip().endswith("-"):
            notes.append(f"Cue {index}: ends with a hyphen; check for a broken word")
    if source:
        original, original_issues = parse_srt(source)
        issues.extend(f"Source {issue}" for issue in original_issues)
        if len(original) != len(cues):
            issues.append(f"Cue counts differ: source {len(original)}, translation {len(cues)}")
        for a, b in zip(original, cues):
            if abs(a["start"] - b["start"]) > 40 or abs(a["end"] - b["end"]) > 40:
                issues.append(f"Cue {b['index']}: source and translation timings differ")
    print(f"{Path(target).name}: {len(cues)} cues")
    if cues:
        print(f"Time span: {cues[0]['start']/1000:.2f}s to {cues[-1]['end']/1000:.2f}s")
    print(f"Structural issues: {len(issues)}; readability review flags: {len(notes)}")
    for issue in issues[:20]:
        print("ERROR", issue)
    for note in notes[:20]:
        print("REVIEW", note)
    if len(issues) > 20 or len(notes) > 20:
        print("Only the first 20 messages of each kind are shown.")
    return bool(issues)


def paths_for_job(job_id, language):
    jobs_file = Path(os.getenv("SUBTITLE_DATA_ROOT", "/data")) / "jobs.json"
    jobs = json.loads(jobs_file.read_text(encoding="utf-8"))
    job = next((item for item in jobs if item.get("id") == job_id), None)
    if not job:
        raise ValueError(f"Job {job_id} was not found")
    output = job.get("outputs", {}).get(language)
    if not output:
        raise ValueError(f"Job {job_id} has no {language.upper()} subtitle")
    original = job.get("outputs", {}).get(job.get("detected_language"), {})
    video = Path(os.getenv("SUBTITLE_LIBRARY_ROOT", "/library")) / job["path"]
    return Path(output["path"]), Path(original["path"]) if original else None, video


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", type=Path, nargs="?", help="SRT file to validate")
    parser.add_argument("--job-id", help="Read subtitle paths from the local job record")
    parser.add_argument("--target-language", default="en", help="Job output language, default en")
    parser.add_argument("--source", type=Path, help="Original-language SRT for alignment checks")
    parser.add_argument("--video", type=Path, help="Video for duration check")
    parser.add_argument("--max-cps", type=float, default=20, help="Target-language reading-speed flag")
    args = parser.parse_args()
    if bool(args.target) == bool(args.job_id):
        parser.error("Provide either an SRT file or --job-id")
    target, source, video = (paths_for_job(args.job_id, args.target_language)
                             if args.job_id else (args.target, None, None))
    raise SystemExit(1 if inspect(target, args.source or source, args.video or video, args.max_cps) else 0)
