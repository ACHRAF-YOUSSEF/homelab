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
        self.assertIn("\n", worker.wrap_subtitle("語" * 80))

    def test_probe_preserves_audio_start_offset(self):
        fixture = {"format": {"duration": "10", "start_time": "0"}, "streams": [{"index": 2, "codec_type": "audio", "start_time": "1.5", "codec_name": "aac", "tags": {"language": "jpn"}}]}
        with patch.object(worker.subprocess, "run", return_value=SimpleNamespace(stdout=json.dumps(fixture))):
            result = worker.probe_media(Path("film.mkv"))
        self.assertEqual(result["tracks"][0]["offset"], 1.5)

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

    def test_translategemma_uses_raw_completions_and_language_template(self):
        class Reply:
            def __enter__(self):
                return self
            def __exit__(self, *args):
                pass
            def read(self):
                return json.dumps({"choices": [{"text": "Hello, world!<end_of_turn>"}]}).encode()
        calls = []
        def fake_open(request, timeout):
            calls.append(request)
            return Reply()
        with patch.object(worker, "urlopen", side_effect=fake_open):
            result = worker.request_translation("mradermacher/translategemma-12b-it-GGUF", "ja", "en", [{"text": "こんにちは、世界！"}])
        self.assertEqual(result, ["Hello, world!"])
        self.assertTrue(calls[0].full_url.endswith("/completions"))
        body = json.loads(calls[0].data)
        self.assertIn("Japanese (ja) to English (en)", body["prompt"])
        self.assertIn("こんにちは、世界！", body["prompt"])
        self.assertNotIn("messages", body)

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
