import os
import tempfile
import time
import unittest
from pathlib import Path, PureWindowsPath
from types import SimpleNamespace
from unittest.mock import patch

import worker


class SubtitleWorkerTests(unittest.TestCase):
    def test_srt_timestamps_and_nonoverlap(self):
        cues = worker.normalize_cues([
            {"start": 0.0, "end": 2.1, "text": "  Hello  there. "},
            {"start": 2.0, "end": 3.25, "text": "How are you?"},
        ])
        self.assertEqual(cues[0]["end"], 2.0)
        self.assertEqual(
            worker.render_srt(cues),
            "1\n00:00:00,000 --> 00:00:02,000\nHello there.\n\n"
            "2\n00:00:02,000 --> 00:00:03,250\nHow are you?\n",
        )

    def test_long_segment_uses_word_timestamps(self):
        words = [
            SimpleNamespace(start=i * 0.5, end=(i + 1) * 0.5, word=" example")
            for i in range(16)
        ]
        segment = SimpleNamespace(start=0, end=8, text=" ".join("example" for _ in words), words=words)
        cues = worker.split_segment(segment)
        self.assertGreater(len(cues), 1)
        self.assertEqual(cues[0]["start"], 0)
        self.assertEqual(cues[-1]["end"], 8)
        self.assertTrue(all(cues[i]["end"] <= cues[i + 1]["start"] for i in range(len(cues) - 1)))

    def test_scan_waits_for_stable_file_and_queues_once(self):
        with tempfile.TemporaryDirectory() as directory:
            input_dir = Path(directory) / "input"
            output_dir = Path(directory) / "output"
            input_dir.mkdir()
            output_dir.mkdir()
            source = input_dir / "film.mkv"
            source.write_bytes(b"mkv fixture")
            old_time = time.time() - 120
            os.utime(source, (old_time, old_time))
            with patch.object(worker, "INPUT_DIR", input_dir), patch.object(worker, "OUTPUT_DIR", output_dir), patch.object(worker, "MIN_AGE_SECONDS", 90):
                worker.seen.clear()
                worker.jobs.clear()
                self.assertEqual(worker.scan(), [])
                queued = worker.scan()
                self.assertEqual(len(queued), 1)
                self.assertEqual(worker.scan(), [])
                self.assertEqual(queued[0]["filename"], "film.mkv")

    def test_proofreading_preserves_timing(self):
        cues = [{"start": 1.25, "end": 2.75, "text": "He go home."}]

        def fake_request(url, payload=None, headers=None):
            if url.endswith("/models"):
                return {"data": [{"id": "local-model"}]}
            return {"choices": [{"message": {"content": '[{"id": 0, "text": "He went home."}]'}}]}

        with patch.object(worker, "request_json", side_effect=fake_request):
            worker.polish(cues)
        self.assertEqual(cues, [{"start": 1.25, "end": 2.75, "text": "He went home."}])

    def test_windows_path_maps_only_inside_mounted_source(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "Movie.mkv"
            source.write_bytes(b"fixture")
            with patch.object(worker, "SOURCE_DIR", Path(directory)), patch.object(worker, "SOURCE_HOST_DIR", PureWindowsPath("D:/subtitles/input")):
                self.assertEqual(worker.resolve_source(r"D:\subtitles\input\Movie.mkv"), source)
                with self.assertRaises(ValueError):
                    worker.resolve_source(r"D:\film\Movie.mkv")
                with self.assertRaises(ValueError):
                    worker.resolve_source(r"D:\subtitles\input\..\..\secret.mkv")

    def test_progress_and_duplicate_job(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "Movie.mkv"
            source.write_bytes(b"fixture")
            with patch.object(worker, "OUTPUT_DIR", root):
                worker.jobs.clear()
                first = worker.enqueue_source(source)
                second = worker.enqueue_source(source)
                self.assertEqual(first["id"], second["id"])
                worker.update_job(first["id"], "transcribing", 55, media_position_seconds=33)
                self.assertEqual(worker.jobs[first["id"]]["progress_percent"], 55)
                self.assertEqual(worker.jobs[first["id"]]["media_position_seconds"], 33)


if __name__ == "__main__":
    unittest.main()
