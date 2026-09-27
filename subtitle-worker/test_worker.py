import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import worker


class SubtitleStudioTests(unittest.TestCase):
    def test_library_path_stays_inside_mount(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            film = root / "film.mkv"
            film.write_bytes(b"fixture")
            with patch.object(worker, "LIBRARY_ROOT", root):
                self.assertEqual(worker.resolve_media("film.mkv", True), film)
                for invalid in ("../outside.mkv", "/outside.mkv", r"..\outside.mkv"):
                    with self.assertRaises(ValueError):
                        worker.resolve_media(invalid, True)

    def test_word_timing_and_srt(self):
        words = [SimpleNamespace(start=i * .5, end=(i + 1) * .5, word=" example") for i in range(16)]
        segment = SimpleNamespace(start=0, end=8, text="", words=words)
        cues = worker.make_cues([segment])
        self.assertGreater(len(cues), 1)
        self.assertEqual(cues[0]["start"], 0)
        self.assertEqual(cues[-1]["end"], 8)
        self.assertTrue(all(cues[i]["end"] <= cues[i + 1]["start"] for i in range(len(cues)-1)))
        self.assertIn("00:00:00,000 -->", worker.render_srt(cues))

    def test_translation_preserves_cue_count_and_order(self):
        class Reply:
            def __enter__(self):
                return self
            def __exit__(self, *args):
                pass
            def read(self):
                return json.dumps({"choices": [{"message": {"content": '[{"id":2,"text":"Deux"},{"id":1,"text":"Un"}]'}}]}).encode()
        cues = [{"text": "One"}, {"text": "Two"}]
        with patch.object(worker, "urlopen", return_value=Reply()):
            self.assertEqual(worker.request_translation("local", "en", "fr", cues), ["Un", "Deux"])

    def test_job_validates_actual_audio_stream(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "film.mkv").write_bytes(b"fixture")
            with patch.object(worker, "LIBRARY_ROOT", root), patch.object(worker, "DATA_ROOT", root), patch.object(worker, "JOBS_FILE", root / "jobs.json"), patch.object(worker, "probe_media", return_value={"tracks": [{"index": 2}], "duration": 30}):
                with self.assertRaises(ValueError):
                    worker.create_job({"path": "film.mkv", "audio_stream_index": 0, "transcript": True})
                job = worker.create_job({"path": "film.mkv", "audio_stream_index": 2, "transcript": True, "targets": ["fr"]})
                self.assertEqual(job["status"], "queued")
                self.assertEqual(json.loads((root / "jobs.json").read_text())[0]["id"], job["id"])
                worker.jobs.clear()


if __name__ == "__main__":
    unittest.main()
