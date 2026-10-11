"""Behavioral tests for the subtitle converter's filesystem safety rules."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from converter import Converter, candidates, run_command


SRT = "1\n00:00:01,000 --> 00:00:02,000\nExample subtitle\n"


class ConverterTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.library = self.base / "library"
        self.library.mkdir()
        self.state = self.base / "state.json"
        self.video = self.library / "Example.mkv"
        self.video.write_bytes(b"stable source")
        # Inputs are older than the settling window used by scan().
        old = time.time() - 600
        os.utime(self.video, (old, old))
        self.converter = Converter([self.library], self.state, min_age=0, retry_interval=30)

    def test_second_scan_uses_cache_without_running_conversion_again(self):
        calls = []
        def convert(path):
            calls.append(path.name)
            (path.parent / "Example.en.srt").write_text(SRT, encoding="utf-8")
            return ["Example.en.srt"]
        self.converter.convert = convert

        first = self.converter.scan()
        second = self.converter.scan()

        self.assertEqual(first["converted"], 1)
        self.assertEqual(second["cached"], 1)
        self.assertEqual(calls, [self.video.name])

    def test_missing_cached_output_retries_conversion(self):
        calls = []
        def convert(path):
            calls.append(path.name)
            (path.parent / "Example.en.srt").write_text(SRT, encoding="utf-8")
            return ["Example.en.srt"]
        self.converter.convert = convert
        self.converter.scan()
        (self.library / "Example.en.srt").unlink()

        result = self.converter.scan()

        self.assertEqual(result["converted"], 1)
        self.assertEqual(len(calls), 2)

    def test_no_subtitle_tracks_are_cached_as_success(self):
        calls = []
        self.converter.convert = lambda path: calls.append(path.name) or []

        self.converter.scan()
        again = self.converter.scan()

        self.assertEqual(again["cached"], 1)
        self.assertEqual(calls, [self.video.name])

    def test_source_change_during_conversion_does_not_record_success(self):
        self.video.unlink()
        source = self.library / "captions.ass"
        source.write_text("original subtitle source", encoding="utf-8")
        old = time.time() - 600
        os.utime(source, (old, old))

        def mutate_source(command, timeout=None):
            source.write_text("changed while converting", encoding="utf-8")
            staging = Path(command[command.index("--output-folder") + 1])
            name = command[command.index("--output-filename") + 1]
            (staging / name).write_text(SRT, encoding="utf-8")
            return json.dumps({"success": True}).encode()

        self.converter.run = mutate_source
        with patch.object(self.converter, "tracks", return_value=[]):
            result = self.converter.scan()

        self.assertEqual(result["failed"], 1)
        self.assertFalse(list(self.library.glob("captions*.srt")))
        self.assertFalse(self.converter.state[str(source)].get("ok", False))

    def test_existing_destination_and_symlink_are_preserved(self):
        existing = self.library / "Example.en.srt"
        existing.write_text("user subtitle\n", encoding="utf-8")
        target = self.base / "outside.srt"
        target.write_text("outside data\n", encoding="utf-8")
        link = self.library / "Example.fr.srt"
        try:
            link.symlink_to(target)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlinks unavailable: {type(error).__name__}")

        def write_conversion(path):
            self.assertEqual(existing.read_text(encoding="utf-8"), "user subtitle\n")
            self.assertTrue(link.is_symlink())
            self.assertEqual(target.read_text(encoding="utf-8"), "outside data\n")
            return [existing.name, link.name]

        self.converter.convert = write_conversion
        self.converter.scan()
        self.assertEqual(existing.read_text(encoding="utf-8"), "user subtitle\n")
        self.assertEqual(target.read_text(encoding="utf-8"), "outside data\n")

    def test_sidecar_source_symlink_is_not_scanned(self):
        outside = self.base / "outside.ass"
        outside.write_text("outside", encoding="utf-8")
        link = self.library / "linked.ass"
        try:
            link.symlink_to(outside)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlinks unavailable: {type(error).__name__}")
        self.assertNotIn(link.resolve(), list(candidates([self.library])))

    def test_vobsub_pair_is_a_single_candidate(self):
        (self.library / "Example.idx").write_text("idx", encoding="ascii")
        (self.library / "Example.sub").write_bytes(b"sub")
        found = list(candidates([self.library]))
        vobsub = [path for path in found if path.suffix.lower() in {".idx", ".sub"}]
        self.assertEqual(vobsub, [self.library / "Example.sub"])

    def test_video_jobs_keep_all_languages_track_ids_and_flags(self):
        tracks = [
            {"number": 2, "language": "eng", "forced": False, "hi": False},
            {"number": 4, "language": "fra", "forced": True, "hi": True},
            {"number": 6, "language": "eng", "forced": False, "hi": False},
        ]
        with patch.object(self.converter, "tracks", return_value=tracks):
            jobs = self.converter.jobs(self.video, self.base / "stage")
        self.assertEqual([job[1] for job in jobs], [
            "Example.track2.en.srt", "Example.fr.forced.hi.srt", "Example.track6.en.srt",
        ])
        self.assertEqual([job[2] for job in jobs], [2, 4, 6])
        self.assertEqual([job[3] for job in jobs], ["eng", "fra", "eng"])

    def test_vobsub_jobs_split_language_sections(self):
        idx = self.library / "Example.idx"
        idx.write_text(
            "# VobSub index file\nsize: 720x480\nlangidx: 0\nid: en, index: 0\n"
            "timestamp: 00:00:01:000, filepos: 000000001\n"
            "id: fr, index: 1\ntimestamp: 00:00:02:000, filepos: 000000002\n",
            encoding="utf-8",
        )
        (self.library / "Example.sub").write_bytes(b"binary subtitle payload")
        stage = self.base / "stage"
        stage.mkdir()
        jobs = self.converter.jobs(idx, stage)
        self.assertEqual([job[1] for job in jobs], ["Example.en.srt", "Example.fr.srt"])
        self.assertTrue(all(job[0].is_file() for job in jobs))
        self.assertTrue((stage / "vobsub0.sub").is_file())
        self.assertTrue((stage / "vobsub1.sub").is_file())

    def test_run_command_kills_process_group_when_cancelled(self):
        process = unittest.mock.Mock()
        process.poll.return_value = None
        process.wait.return_value = None
        stopped = threading.Event()
        stopped.set()
        with patch("converter.subprocess.Popen", return_value=process), \
             patch("converter.os.name", "posix"), \
             patch("converter.os.killpg", create=True) as kill_group, \
             patch("converter.signal.SIGKILL", 9, create=True):
            with self.assertRaises(TimeoutError):
                run_command(["tool"], timeout=30, stop=stopped)
        kill_group.assert_called_once()

    def test_partial_or_invalid_seconv_output_publishes_nothing(self):
        calls = []
        def seconv_result(command, timeout=None):
            staging = Path(command[command.index("--output-folder") + 1])
            name = command[command.index("--output-filename") + 1]
            calls.append(name)
            content = SRT if len(calls) == 1 else "partial"
            (staging / name).write_text(content, encoding="utf-8")
            return json.dumps({"success": True}).encode()

        self.converter.run = seconv_result
        tracks = [
            {"number": 2, "language": "eng", "forced": False, "hi": False},
            {"number": 3, "language": "fra", "forced": False, "hi": False},
        ]
        with patch.object(self.converter, "tracks", return_value=tracks):
            with self.assertRaises(ValueError):
                self.converter.convert(self.video)
        self.assertEqual(calls, ["Example.en.srt", "Example.fr.srt"])
        self.assertFalse((self.library / "Example.en.srt").exists())
        self.assertFalse((self.library / "Example.fr.srt").exists())

    def test_failed_seconv_publishes_nothing(self):
        source = self.library / "captions.vtt"
        source.write_text("WEBVTT", encoding="utf-8")
        def failed(command, timeout=None):
            raise RuntimeError("mock tool failure")

        self.converter.run = failed
        with patch.object(self.converter, "tracks", return_value=[]):
            with self.assertRaises(RuntimeError):
                self.converter.convert(source)
        self.assertFalse((self.library / "captions.srt").exists())

    def test_retry_waits_after_failure_then_source_change_retries(self):
        calls = []
        def fail(path):
            calls.append(path.name)
            raise RuntimeError("mock conversion failure")
        self.converter.convert = fail
        self.converter.scan()
        second = self.converter.scan()
        self.assertEqual(second["pending"], 1)
        self.assertEqual(len(calls), 1)
        self.video.write_bytes(b"new source version")
        result = self.converter.scan()
        self.assertEqual(result["failed"], 1)
        self.assertEqual(len(calls), 2)


if __name__ == "__main__":
    unittest.main()
