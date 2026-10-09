import copy
import io
import json
import queue
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import worker


class StepActionTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        (self.root / "film.mkv").write_bytes(b"media fixture")
        self.patches = [patch.object(worker, "LIBRARY_ROOT", self.root),
                        patch.object(worker, "DATA_ROOT", self.root / "data"),
                        patch.object(worker, "OUTPUT_ROOT", self.root / "output"),
                        patch.object(worker, "JOBS_FILE", self.root / "data" / "jobs.json"),
                        patch.object(worker, "jobs", {}), patch.object(worker, "job_queue", queue.Queue()),
                        patch.object(worker, "REVIEW_DEFAULT", ""), patch.object(worker, "LLM_DEFAULT", ""),
                        patch.object(worker, "probe_media", return_value={"tracks": [{"index": 1}], "duration": 10})]
        for item in self.patches:
            item.start()
        self.segments = [SimpleNamespace(start=index + .1234, end=index + .8, text=f"source {index}", words=[])
                         for index in range(9)]
        self.speech = SimpleNamespace(transcribe=lambda *args, **kwargs: (iter(self.segments), SimpleNamespace(language="ja")))

    def tearDown(self):
        for item in reversed(self.patches):
            item.stop()
        self.directory.cleanup()

    def create(self, **options):
        return worker.create_job({"path": "film.mkv", "audio_stream_index": 1, "source_language": "ja",
                                  "targets": ["en", "fr"], "transcript": True, "model": "translator",
                                  "review_enabled": False, **options})

    def extract(self, media, index, destination, job_id=None):
        destination.write_bytes(b"RIFF" + b"\0" * 80)

    def translate(self, model, source, target, cues):
        return [f"{target} {cue['text']}" for cue in cues]

    def review(self, model, source, target, cues, texts, start):
        return [{"id": index + 1, "fidelity": 90, "fluency": 95, "issue": ""}
                for index in range(start, min(start + worker.REVIEW_BATCH_SIZE, len(cues)))]

    def drain(self):
        packets = []
        while not worker.job_queue.empty():
            packets.append(worker.job_queue.get_nowait())
        finite = Mock()
        finite.get.side_effect = [*packets, StopIteration]
        with patch.object(worker, "job_queue", finite):
            with self.assertRaises(StopIteration):
                worker.worker_loop()
        self.assertEqual(finite.task_done.call_count, len(packets))

    def finish(self, job):
        with (patch.object(worker, "extract_audio", side_effect=self.extract),
              patch.object(worker, "get_speech_model", return_value=self.speech),
              patch.object(worker, "request_translation", side_effect=self.translate),
              patch.object(worker, "request_quality_review", side_effect=self.review)):
            self.drain()

    def test_queued_core_cancel_skips_old_packet_and_retries_only_selected_core_step(self):
        job = self.create()
        worker.cancel_step(job["id"], "extraction")
        self.assertEqual(job["status"], "cancelled")
        self.assertEqual(worker.public_job(job)["steps"]["transcription"]["status"], "blocked")
        worker.retry_step(job["id"], "extraction")
        with (patch.object(worker, "extract_audio", side_effect=self.extract) as extract,
              patch.object(worker, "get_speech_model", return_value=self.speech) as speech,
              patch.object(worker, "request_translation") as translate):
            self.drain()
            self.assertEqual(extract.call_count, 1)
            speech.assert_not_called()
            translate.assert_not_called()
            worker.retry_step(job["id"], "transcription")
            self.drain()
            self.assertEqual(extract.call_count, 1)
            self.assertEqual(speech.call_count, 1)
            translate.assert_not_called()
        self.assertEqual(set(job["outputs"]), {"ja"})
        self.assertEqual(job["targets"], ["en", "fr"])
        self.assertEqual(job["step_task"]["status"], "completed")
        self.assertTrue(worker.public_job(job)["steps"]["translation:en"]["can_retry"])

    def test_cancel_translation_saves_finished_batch_continues_french_and_retry_resumes(self):
        job = self.create()
        calls = []
        def translate(model, source, target, cues):
            calls.append((target, len(cues)))
            if target == "en" and len(calls) == 1:
                cancelled = worker.cancel_step(job["id"], "translation:en")
                self.assertEqual(cancelled["steps"]["translation:en"]["status"], "cancelling")
            return self.translate(model, source, target, cues)
        with (patch.object(worker, "extract_audio", side_effect=self.extract),
              patch.object(worker, "get_speech_model", return_value=self.speech),
              patch.object(worker, "request_translation", side_effect=translate)):
            self.drain()
        self.assertEqual(calls, [("en", 8), ("fr", 8), ("fr", 1)])
        self.assertEqual(set(job["outputs"]), {"ja", "fr"})
        self.assertEqual(job["status"], "completed")
        self.assertEqual(job["steps"]["translation:en"]["status"], "cancelled")
        french = Path(job["outputs"]["fr"]["path"]).read_bytes()
        worker.retry_step(job["id"], "translation:en")
        with self.assertRaises(worker.ReviewConflict):
            worker.retry_step(job["id"], "translation:en")
        with (patch.object(worker, "extract_audio") as extract, patch.object(worker, "get_speech_model") as speech,
              patch.object(worker, "request_translation", side_effect=translate)):
            self.drain()
            extract.assert_not_called()
            speech.assert_not_called()
        self.assertEqual(calls[-1], ("en", 1))
        self.assertEqual(Path(job["outputs"]["fr"]["path"]).read_bytes(), french)
        self.assertEqual(job["targets"], ["en", "fr"])
        self.assertEqual(job["steps"]["translation:en"]["status"], "completed")

    def test_cancel_pending_translation_skips_language_before_any_request(self):
        job = self.create()
        worker.cancel_step(job["id"], "translation:en")
        with (patch.object(worker, "extract_audio", side_effect=self.extract),
              patch.object(worker, "get_speech_model", return_value=self.speech),
              patch.object(worker, "request_translation", side_effect=self.translate) as translator):
            self.drain()
        self.assertEqual([call.args[2] for call in translator.call_args_list], ["fr", "fr"])
        self.assertEqual(set(job["outputs"]), {"ja", "fr"})

    def test_whisper_cancel_stops_dependencies_and_retry_reuses_cached_audio(self):
        job = self.create()
        def segments():
            yield self.segments[0]
            worker.cancel_step(job["id"], "transcription")
            yield self.segments[1]
        speech = SimpleNamespace(transcribe=lambda *a, **k: (segments(), SimpleNamespace(language="ja")))
        with (patch.object(worker, "extract_audio", side_effect=self.extract) as extract,
              patch.object(worker, "get_speech_model", return_value=speech),
              patch.object(worker, "request_translation") as translator):
            self.drain()
            self.assertEqual(job["status"], "cancelled")
            self.assertEqual(job["steps"]["transcription"]["status"], "cancelled")
            translator.assert_not_called()
            self.assertIsNone(worker.load_transcript_cache(job["transcript_key"]))
            worker.retry_step(job["id"], "transcription")
            with patch.object(worker, "get_speech_model", return_value=self.speech):
                self.drain()
            self.assertEqual(extract.call_count, 1)
            translator.assert_not_called()
        self.assertIsNotNone(worker.load_transcript_cache(job["transcript_key"]))

    def test_review_cancel_continues_other_language_and_retries_partial_cache(self):
        job = self.create(review_enabled=True)
        calls = []
        def review(model, source, target, cues, texts, start):
            calls.append((target, start))
            if target == "en" and start == 0:
                worker.cancel_step(job["id"], "review:en")
            return self.review(model, source, target, cues, texts, start)
        with (patch.object(worker, "extract_audio", side_effect=self.extract),
              patch.object(worker, "get_speech_model", return_value=self.speech),
              patch.object(worker, "request_translation", side_effect=self.translate),
              patch.object(worker, "request_quality_review", side_effect=review)):
            self.drain()
        self.assertEqual(calls, [("en", 0), ("fr", 0), ("fr", 8)])
        self.assertEqual(job["quality"]["fr"]["status"], "completed")
        self.assertNotIn("score", job["quality"]["en"])
        worker.retry_step(job["id"], "review:en")
        with patch.object(worker, "request_quality_review", side_effect=review):
            self.drain()
        self.assertEqual(calls[-1], ("en", 8))
        self.assertEqual(job["quality"]["en"]["score"], 90)

    def test_whole_review_cancel_clears_running_report_and_keeps_existing_score(self):
        job = self.create()
        self.finish(job)
        job["quality"]["fr"] = {"status": "completed", "score": 93, "fluency": 95}
        previous = copy.deepcopy(job["quality"]["fr"])
        worker.queue_review(job["id"], {"languages": ["en", "fr"]})
        def review(model, source, target, cues, texts, start):
            worker.cancel_job(job["id"])
            return self.review(model, source, target, cues, texts, start)
        with patch.object(worker, "request_quality_review", side_effect=review):
            self.drain()
        self.assertEqual(job["review_task"]["status"], "cancelled")
        self.assertEqual(job["quality"]["en"]["status"], "unavailable")
        self.assertNotIn("score", job["quality"]["en"])
        self.assertEqual(job["quality"]["fr"], previous)
        self.assertFalse(any(step["status"] == "cancelling" for step in job["steps"].values()))
        self.assertEqual(job["status"], "completed")

    def test_cancelled_queued_review_packet_cannot_execute_new_retry(self):
        job = self.create()
        self.finish(job)
        worker.queue_review(job["id"], {"languages": ["en"]})
        old = worker.job_queue.get_nowait()
        worker.cancel_step(job["id"], "review:en")
        worker.retry_step(job["id"], "review:en")
        new = worker.job_queue.get_nowait()
        self.assertNotEqual(old["token"], new["token"])
        worker.job_queue.put(old)
        worker.job_queue.put(new)
        with patch.object(worker, "request_quality_review", side_effect=self.review) as reviewer:
            self.drain()
        self.assertEqual(reviewer.call_count, 2)
        self.assertEqual(job["quality"]["en"]["score"], 90)

    def test_restart_fails_step_retry_without_changing_original_generation_result(self):
        job = self.create()
        self.finish(job)
        worker.set_step(job["id"], "translation:en", status="failed", error="Old translation attempt failed")
        worker.retry_step(job["id"], "translation:en")
        worker.jobs.clear()
        worker.load_jobs()
        restored = worker.jobs[job["id"]]
        self.assertEqual(restored["status"], "completed")
        self.assertEqual(restored["step_task"]["status"], "failed")
        self.assertEqual(restored["steps"]["translation:en"]["status"], "failed")

    def test_late_terminal_cancel_is_rejected_and_source_only_auto_steps_disappear(self):
        job = self.create(source_language="auto", targets=["ja", "en"])
        self.finish(job)
        self.assertNotIn("translation:ja", worker.public_job(job)["steps"])
        self.assertNotIn("review:ja", worker.public_job(job)["steps"])
        job["status"] = "running"  # final completion publication boundary
        with self.assertRaises(worker.ReviewConflict):
            worker.cancel_job(job["id"])
        with self.assertRaises(worker.ReviewConflict):
            worker.cancel_step(job["id"], "translation:en")
        self.assertFalse(job["cancel_requested"])

    def test_ffmpeg_process_terminates_at_cancellation_poll(self):
        job = self.create()
        worker.set_job(job["id"], status="running")
        worker.set_step(job["id"], "extraction", status="running")
        process = Mock(returncode=None)
        process.poll.return_value = None
        def communicate(timeout):
            if process.returncode is None:
                worker.cancel_step(job["id"], "extraction")
                raise worker.subprocess.TimeoutExpired("ffmpeg", timeout)
            return None, b""
        process.communicate.side_effect = communicate
        process.terminate.side_effect = lambda: setattr(process, "returncode", 0)
        with patch.object(worker.subprocess, "Popen", return_value=process):
            with self.assertRaises(worker.StepCancelled):
                worker.extract_audio(self.root / "film.mkv", 1, self.root / "audio.wav", job_id=job["id"])
        process.terminate.assert_called_once()
        process.kill.assert_not_called()

    def test_failed_http_attempt_observes_cancel_before_retrying_model(self):
        job = self.create(review_enabled=True)
        worker.set_job(job["id"], status="running")
        worker.set_step(job["id"], "review:en", status="running")
        class Reply:
            def __enter__(reply):
                return reply
            def __exit__(reply, *args):
                pass
            def read(reply):
                worker.cancel_step(job["id"], "review:en")
                return json.dumps({"choices": [{"message": {"content": "invalid json"}}]}).encode()
        cues = worker.make_cues(self.segments)
        with patch.object(worker, "open_http", return_value=Reply()) as request, patch.object(worker.time, "sleep") as sleep:
            with self.assertRaises(worker.StepCancelled):
                worker.call_step_model(job["id"], "review:en", worker.request_quality_review, "translator", "ja", "en", cues, ["text"] * 9, 0)
        request.assert_called_once()
        sleep.assert_not_called()

    def test_step_http_routes_validate_actions_and_conflicts(self):
        job = self.create()
        def post(suffix, payload):
            handler = worker.Handler.__new__(worker.Handler)
            body = json.dumps(payload).encode()
            handler.path = f"/api/jobs/{job['id']}/{suffix}"
            handler.headers, handler.rfile = {"Content-Length": str(len(body))}, io.BytesIO(body)
            result = []
            handler.send_json = lambda value, status=200: result.append((value, status))
            handler.do_POST()
            return result[0]
        self.assertEqual(post("steps/cancel", {"step": 1})[1], 400)
        self.assertEqual(post("steps/cancel", {"step": "unknown"})[1], 400)
        self.assertEqual(post("steps/retry", {"step": "translation:en"})[1], 409)
        self.assertEqual(post("cancel", {})[1], 202)
        self.assertEqual(post("cancel", {})[1], 409)


if __name__ == "__main__":
    unittest.main()
