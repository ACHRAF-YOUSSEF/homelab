"""Behavioral tests for the subtitle converter's filesystem safety rules."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from converter import Converter, candidates, remove_empty_srt_cues, run_command, valid_srt


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

    def test_selected_language_scans_expand_cache_without_replacing_outputs(self):
        tracks = [
            {"number": 2, "language": "eng", "forced": False, "hi": False},
            {"number": 3, "language": "fra", "forced": False, "hi": False},
        ]
        calls = []
        progress = []

        def run(command, timeout=None, *, check=True):
            name = command[command.index("--output-filename") + 1]
            staging = Path(command[command.index("--output-folder") + 1])
            calls.append(name)
            (staging / name).write_text(SRT.replace("Example subtitle", name), encoding="utf-8")
            return json.dumps({"success": True}).encode()

        self.converter.run = run
        self.converter.progress = progress.append
        with patch.object(self.converter, "tracks", return_value=tracks):
            self.converter.selected_languages = ["en"]
            english = self.converter.scan([self.video])
            en_before = (self.library / "Example.en.srt").read_bytes()
            self.assertEqual(english["converted"], 1)
            self.assertEqual(self.converter.state[str(self.video)]["languages"], ["en"])
            self.assertEqual(self.converter.state[str(self.video)]["outputs"], ["Example.en.srt"])

            self.converter.selected_languages = ["fr"]
            french = self.converter.scan([self.video])
            self.assertEqual(french["converted"], 1)
            self.assertEqual(self.converter.state[str(self.video)]["languages"], ["en", "fr"])
            self.assertEqual(self.converter.state[str(self.video)]["outputs"], ["Example.en.srt", "Example.fr.srt"])
            self.assertEqual((self.library / "Example.en.srt").read_bytes(), en_before)
            fr_events = [event for event in progress if event.get("status") == "converted"][-1]
            self.assertEqual(fr_events["new_outputs"], [str(self.library / "Example.fr.srt")])

            self.converter.selected_languages = ["*"]
            all_languages = self.converter.scan([self.video])
            self.assertEqual(all_languages["converted"], 1)
            self.assertEqual(self.converter.state[str(self.video)]["outputs"], ["Example.en.srt", "Example.fr.srt"])
            self.assertEqual((self.library / "Example.en.srt").read_bytes(), en_before)
            self.assertEqual(calls, ["Example.en.srt", "Example.fr.srt"])

            cached = self.converter.scan([self.video])
            self.assertEqual(cached["cached"], 1)
            self.assertEqual(calls, ["Example.en.srt", "Example.fr.srt"])

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

        def mutate_source(command, timeout=None, *, check=True):
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

    def test_run_command_can_return_structured_failure_json(self):
        command = [sys.executable, "-c", "import sys; print('{\"success\": false}'); sys.exit(1)"]
        result = run_command(command, timeout=5, check=False)
        self.assertEqual(json.loads(result), {"success": False})
        with self.assertRaisesRegex(RuntimeError, "Local conversion tool failed"):
            run_command(command, timeout=5)

    def test_partial_or_invalid_seconv_output_publishes_nothing(self):
        calls = []
        def seconv_result(command, timeout=None, *, check=True):
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

    def test_removes_only_well_formed_empty_cues_from_staged_srt(self):
        staged = self.base / "staged.srt"
        staged.write_text(
            "1\n00:00:00,000 --> 00:00:00,530\n\n"
            "2\n00:00:01,000 --> 00:00:02,000\nVisible line\n\n"
            "7\n00:00:02,000 --> 00:00:03,000\n \t\n",
            encoding="utf-8",
        )
        self.assertFalse(valid_srt(staged))
        self.assertEqual(remove_empty_srt_cues(staged), 2)
        self.assertTrue(valid_srt(staged))
        self.assertEqual(staged.read_text(encoding="utf-8"),
                         "1\n00:00:01,000 --> 00:00:02,000\nVisible line\n")

    def test_malformed_empty_cue_is_not_removed(self):
        staged = self.base / "staged.srt"
        malformed = "1\nnot a timestamp\n"
        staged.write_text(malformed, encoding="utf-8")
        self.assertEqual(remove_empty_srt_cues(staged), 0)
        self.assertEqual(staged.read_text(encoding="utf-8"), malformed)
        self.assertFalse(valid_srt(staged))

    def test_cleanup_does_not_repair_a_malformed_kept_cue(self):
        staged = self.base / "staged.srt"
        staged.write_text(
            "1\n00:00:00,000 --> 00:00:00,530\n\n"
            "invalid-index\n00:00:01,000 --> 00:00:02,000\nVisible text\n",
            encoding="utf-8",
        )
        self.assertEqual(remove_empty_srt_cues(staged), 1)
        self.assertIn("invalid-index\n00:00:01,000", staged.read_text(encoding="utf-8"))
        self.assertFalse(valid_srt(staged))

    def test_failed_seconv_publishes_nothing(self):
        source = self.library / "captions.vtt"
        source.write_text("WEBVTT", encoding="utf-8")
        def failed(command, timeout=None, *, check=True):
            self.assertFalse(check)
            return json.dumps({"success": False, "errors": [
                "OCR failed to load /private/folder/tesseract.token=secret.traineddata"
            ]}).encode()

        self.converter.run = failed
        with patch.object(self.converter, "tracks", return_value=[]):
            with self.assertRaisesRegex(ValueError, "OCR failed. Check that the selected language data is installed and supported"):
                self.converter.convert(source)
        self.assertFalse((self.library / "captions.srt").exists())

    def test_safe_seconv_failure_is_reported_without_raw_tool_details(self):
        events = []
        self.converter.progress = events.append
        self.converter.run = lambda command, timeout=None, *, check=True: json.dumps({
            "success": False,
            "errors": ["permission denied at C:\\Users\\private\\media\\movie.mkv token=secret"],
        }).encode()
        with patch.object(self.converter, "tracks", return_value=[
            {"number": 2, "language": "eng", "forced": False, "hi": False},
        ]):
            result = self.converter.scan([self.video])
        failed = [event for event in events if event.get("status") == "failed"]
        self.assertEqual(result["failed"], 1)
        self.assertEqual(len(failed), 1)
        self.assertEqual(failed[0]["error"],
                         "Subtitle Edit could not write its staged output. Check folder permissions and free space.")
        self.assertEqual(self.converter.state[str(self.video)]["error"], failed[0]["error"])
        self.assertNotIn("private", failed[0]["error"])
        self.assertNotIn("secret", failed[0]["error"])

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
