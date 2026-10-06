import copy
import io
import json
import queue
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import worker


def mock_ratings(model, source, target, cues, texts, start):
    return [{"id": index + 1, "fidelity": 91, "fluency": 95, "issue": ""}
            for index in range(start, min(start + worker.REVIEW_BATCH_SIZE, len(cues)))]


class ExistingReviewTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.data = self.root / "data"
        self.output = self.root / "output"
        self.patches = [patch.object(worker, "DATA_ROOT", self.data),
                        patch.object(worker, "OUTPUT_ROOT", self.output),
                        patch.object(worker, "JOBS_FILE", self.data / "jobs.json"),
                        patch.object(worker, "LIBRARY_ROOT", self.root / "unavailable_media"),
                        patch.object(worker, "job_queue", queue.Queue()),
                        patch.object(worker, "jobs", {}),
                        patch.object(worker, "REVIEW_DEFAULT", ""),
                        patch.object(worker, "LLM_DEFAULT", "")]
        for item in self.patches:
            item.start()
        self.cues = [{"start": index + .1234, "end": index + .8, "text": f"source {index}"}
                     for index in range(9)]

    def tearDown(self):
        for item in reversed(self.patches):
            item.stop()
        self.directory.cleanup()

    def job(self, identifier="012345abcdef", source=True, status="completed", targets=("en",)):
        saved = self.data / "job_outputs" / identifier
        saved.mkdir(parents=True)
        outputs = {}
        for code in ("ja", *targets) if source else targets:
            path = saved / f"film.{code}.srt"
            cues = self.cues if code == "ja" else [{**cue, "text": f"{code} translation {index}"}
                                                  for index, cue in enumerate(self.cues)]
            path.write_text(worker.render_srt(cues), encoding="utf-8")
            outputs[code] = {"path": str(path), "name": path.name}
        job = {"id": identifier, "filename": "film.mkv", "path": "film.mkv",
               "source_language": "ja", "detected_language": "ja", "targets": list(targets),
               "model": "translator", "outputs": outputs, "status": status,
               "stage": "Generation failed" if status == "failed" else "Complete",
               "progress": 73 if status == "failed" else 100,
               "error": "Translation of FR failed" if status == "failed" else None,
               "created_at": worker.now(), "updated_at": worker.now(), "quality": {}, "reused": []}
        worker.jobs[identifier] = job
        return job

    def run_queued_review(self, identifier):
        task = worker.job_queue.get_nowait()
        self.assertEqual((task["kind"], task["id"]), ("review", identifier))
        self.assertEqual(task["token"], worker.jobs[identifier]["review_task"]["token"])
        worker.run_review_task(identifier)

    def test_legacy_saved_srt_review_does_not_touch_media_generation_or_output(self):
        job = self.job(status="failed")
        original = {key: job[key] for key in ("status", "stage", "progress", "error")}
        saved = {code: Path(output["path"]).read_bytes() for code, output in job["outputs"].items()}
        with (patch.object(worker, "get_speech_model") as speech,
              patch.object(worker, "extract_audio") as extract,
              patch.object(worker, "request_translation") as translate,
              patch.object(worker, "request_quality_review", side_effect=mock_ratings) as reviewer):
            worker.queue_review(job["id"], {})
            self.run_queued_review(job["id"])
            self.assertEqual(reviewer.call_count, 2)
            speech.assert_not_called()
            extract.assert_not_called()
            translate.assert_not_called()
        for key, value in original.items():
            self.assertEqual(job[key], value)
        for code, output in job["outputs"].items():
            self.assertEqual(Path(output["path"]).read_bytes(), saved[code])
        self.assertEqual(job["quality"]["en"]["score"], 91)
        self.assertEqual(job["review_task"]["status"], "completed")
        self.assertEqual(worker.public_job(job)["review_available"]["languages"], ["en"])

    def test_source_snapshot_unwraps_layout_without_adding_cjk_spaces(self):
        job = self.job()
        source = "語" * 90
        target = "An ordinary English sentence with enough words to require subtitle line wrapping."
        Path(job["outputs"]["ja"]["path"]).write_text(worker.render_srt([{**self.cues[0], "text": source}]), encoding="utf-8")
        Path(job["outputs"]["en"]["path"]).write_text(worker.render_srt([{**self.cues[0], "text": target}]), encoding="utf-8")
        _, _, cues, translations, _ = worker.review_inputs(job)
        self.assertEqual(cues[0]["text"], source)
        self.assertEqual(translations["en"], [target])

    def test_source_cache_fallback_and_legacy_model_identity(self):
        job = self.job(source=False)
        key = "a" * 64
        job["transcript_key"] = key
        worker.write_cache("transcripts", key, {"language": "ja", "cues": self.cues})
        self.assertEqual(worker.review_available(job)["languages"], ["en"])
        with patch.object(worker, "request_quality_review", side_effect=mock_ratings):
            worker.queue_review(job["id"], {})
            self.run_queued_review(job["id"])
        del job["transcript_key"]
        job.update(audio_stream_index=1, audio_offset=0,
                   media_fingerprint={"path": "film.mkv", "size": 1, "mtime_ns": 2, "sample": "hash"})
        legacy = {**job, "whisper_model": "medium"}
        old_key = worker.transcript_key_from_fingerprint(legacy, job["media_fingerprint"], True)
        worker.write_cache("transcripts", old_key, {"language": "ja", "cues": self.cues})
        with patch.object(worker, "WHISPER_NAME", "large-v3"):
            self.assertEqual(worker.review_available(job)["languages"], ["en"])

    def test_review_only_resumes_original_generation_partial_cache(self):
        job = self.job()
        key = "b" * 64
        job["transcript_key"] = key
        worker.write_cache("transcripts", key, {"language": "ja", "cues": self.cues})
        texts = [f"en translation {index}" for index in range(9)]
        quality_key = worker.quality_cache_key(key, "ja", self.cues, "en", texts, "translator")
        partial = mock_ratings("translator", "ja", "en", self.cues, texts, 0)
        worker.write_cache("quality", quality_key, {"ratings": partial})
        job["quality"]["en"] = worker.quality_report("unavailable", "translator", "translator", self.cues, texts, partial, "Last batch failed")
        with patch.object(worker, "request_quality_review", side_effect=mock_ratings) as reviewer:
            worker.queue_review(job["id"], {})
            self.run_queued_review(job["id"])
            self.assertEqual([call.args[-1] for call in reviewer.call_args_list], [8])
            worker.queue_review(job["id"], {"languages": ["en"]})
            self.run_queued_review(job["id"])
            self.assertEqual(reviewer.call_count, 1)
        self.assertEqual(job["quality"]["en"]["score"], 91)

    def test_changed_saved_source_does_not_reuse_original_generation_review(self):
        job = self.job()
        key = "c" * 64
        job["transcript_key"] = key
        worker.write_cache("transcripts", key, {"language": "ja", "cues": self.cues})
        texts = [f"en translation {index}" for index in range(9)]
        quality_key = worker.quality_cache_key(key, "ja", self.cues, "en", texts, "translator")
        ratings = mock_ratings("translator", "ja", "en", self.cues, texts, 0) + mock_ratings("translator", "ja", "en", self.cues, texts, 8)
        worker.write_cache("quality", quality_key, {"ratings": ratings})
        changed = [{**cue, "text": "Changed source"} if index == 0 else cue for index, cue in enumerate(self.cues)]
        Path(job["outputs"]["ja"]["path"]).write_text(worker.render_srt(changed), encoding="utf-8")
        with patch.object(worker, "request_quality_review", side_effect=mock_ratings) as reviewer:
            worker.queue_review(job["id"], {})
            self.run_queued_review(job["id"])
            self.assertEqual([call.args[-1] for call in reviewer.call_args_list], [0, 8])
            self.assertEqual(reviewer.call_args_list[0].args[3][0]["text"], "Changed source")

    def test_auto_detected_source_outside_output_language_list_can_be_reviewed(self):
        job = self.job()
        job.update(source_language="auto", detected_language="th")
        job["outputs"]["th"] = job["outputs"].pop("ja")
        self.assertEqual(worker.review_available(job)["languages"], ["en"])
        with patch.object(worker, "request_quality_review", side_effect=mock_ratings) as reviewer:
            worker.queue_review(job["id"], {})
            self.run_queued_review(job["id"])
            self.assertEqual(reviewer.call_args.args[1], "th")

    def test_saved_files_and_cache_require_matching_cue_alignment(self):
        job = self.job()
        path = Path(job["outputs"]["en"]["path"])
        changed = [{**cue, "end": cue["end"] + .01, "text": "translated"} for cue in self.cues]
        path.write_text(worker.render_srt(changed), encoding="utf-8")
        self.assertIn("timestamps", worker.review_available(job)["reason"])
        with self.assertRaises(ValueError):
            worker.queue_review(job["id"], {})
        path.write_text(worker.render_srt(changed[:2]), encoding="utf-8")
        self.assertEqual(worker.review_available(job)["languages"], [])
        outside = self.root / "outside.srt"
        outside.write_text(worker.render_srt(self.cues), encoding="utf-8")
        job["outputs"]["en"]["path"] = str(outside)
        self.assertIn("outside", worker.review_available(job)["reason"])

    def test_duplicate_guard_and_payload_validation(self):
        job = self.job()
        worker.queue_review(job["id"], {})
        with self.assertRaises(worker.ReviewConflict):
            worker.queue_review(job["id"], {})
        job["review_task"]["status"] = "completed"
        job["status"] = "running"
        with self.assertRaises(worker.ReviewConflict):
            worker.queue_review(job["id"], {})
        job["status"] = "completed"
        for payload in ({"force": "true"}, {"review_model": 42}, {"review_model": "translategemma"},
                        {"languages": []}, {"languages": ["xx"]}, {"languages": ["fr"]}):
            with self.subTest(payload=payload), self.assertRaises(ValueError):
                worker.queue_review(job["id"], payload)

    def test_cached_repeated_review_and_force_bypasses_cache(self):
        job = self.job()
        with patch.object(worker, "request_quality_review", side_effect=mock_ratings) as reviewer:
            worker.queue_review(job["id"], {})
            self.run_queued_review(job["id"])
            worker.queue_review(job["id"], {"languages": ["en"]})
            self.run_queued_review(job["id"])
            self.assertEqual(reviewer.call_count, 2)
            self.assertIn("en review", job["reused"])
            worker.queue_review(job["id"], {"force": True})
            self.run_queued_review(job["id"])
            self.assertEqual(reviewer.call_count, 4)
        with self.assertRaisesRegex(ValueError, "already have"):
            worker.queue_review(job["id"], {})
        # Identical saved contents share their review even across job IDs.
        other = self.job("fedcba543210")
        with patch.object(worker, "request_quality_review") as reviewer:
            worker.queue_review(other["id"], {})
            self.run_queued_review(other["id"])
            reviewer.assert_not_called()

    def test_failed_forced_rereview_retains_previous_completed_report(self):
        job = self.job()
        with patch.object(worker, "request_quality_review", side_effect=mock_ratings):
            worker.queue_review(job["id"], {})
            self.run_queued_review(job["id"])
        previous = copy.deepcopy(job["quality"]["en"])
        with patch.object(worker, "request_quality_review", side_effect=RuntimeError("Reviewer unavailable")):
            worker.queue_review(job["id"], {"force": True, "review_model": "other"})
            self.run_queued_review(job["id"])
        self.assertEqual(job["quality"]["en"], previous)
        self.assertEqual(job["status"], "completed")
        self.assertEqual(job["review_task"]["status"], "failed")
        self.assertIn("Reviewer unavailable", job["review_task"]["error"])

    def test_failed_forced_rereview_resumes_partial_new_ratings_and_overwrites_cache(self):
        job = self.job(status="failed")
        with patch.object(worker, "request_quality_review", side_effect=mock_ratings):
            worker.queue_review(job["id"], {})
            self.run_queued_review(job["id"])
        previous = copy.deepcopy(job["quality"]["en"])
        calls = []
        def changed_review(model, source, target, cues, texts, start):
            calls.append(start)
            if len(calls) == 2:
                raise RuntimeError("Last batch temporarily failed")
            return [{**row, "fidelity": 50} for row in mock_ratings(model, source, target, cues, texts, start)]
        with patch.object(worker, "request_quality_review", side_effect=changed_review):
            worker.queue_review(job["id"], {"force": True})
            self.run_queued_review(job["id"])
            self.assertEqual(job["quality"]["en"], previous)
            self.assertEqual(job["review_task"]["status"], "failed")
            worker.queue_review(job["id"], {"languages": ["en"]})
            self.run_queued_review(job["id"])
            self.assertEqual(calls, [0, 8, 8])
            self.assertEqual(job["quality"]["en"]["score"], 50)
            worker.queue_review(job["id"], {"languages": ["en"]})
            self.run_queued_review(job["id"])
            self.assertEqual(calls, [0, 8, 8])
        self.assertEqual(job["status"], "failed")
        self.assertEqual(job["progress"], 73)
        self.assertEqual(job["error"], "Translation of FR failed")

    def test_available_exposes_per_language_rejections_without_blocking_other_targets(self):
        job = self.job(targets=("en", "fr"))
        path = Path(job["outputs"]["fr"]["path"])
        changed = [{**cue, "end": cue["end"] + .01, "text": "changed"} for cue in self.cues]
        path.write_text(worker.render_srt(changed), encoding="utf-8")
        available = worker.review_available(job)
        self.assertEqual(available["languages"], ["en"])
        self.assertIsNone(available["reason"])
        self.assertIn("timestamps", available["rejected"]["fr"])
        queued = worker.queue_review(job["id"], {})
        self.assertEqual(queued["review_task"]["languages"], ["en"])

    def test_bulk_only_missing_all_persisted_jobs_and_skips_active_or_ineligible(self):
        completed = self.job("000000000001", targets=("en", "fr"))
        completed["quality"]["en"] = {"status": "completed", "score": 91}
        active = self.job("000000000002", status="running")
        no_targets = self.job("000000000003", targets=())
        no_source = self.job("000000000004", source=False)
        busy = self.job("000000000005")
        busy["review_task"] = {"status": "queued"}
        for index in range(6, 108):
            self.job(f"{index:012x}")
        result = worker.queue_missing_reviews({})
        self.assertEqual(len(result["queued"]), 103)
        self.assertEqual(completed["review_task"]["languages"], ["fr"])
        self.assertEqual({row["id"] for row in result["skipped"]}, {active["id"], no_targets["id"], no_source["id"], busy["id"]})

    def test_restart_interrupts_only_review_task_and_keeps_completed_score(self):
        job = self.job(status="failed")
        job["quality"]["en"] = {"status": "completed", "score": 91}
        worker.queue_review(job["id"], {"force": True})
        worker.jobs.clear()
        worker.load_jobs()
        loaded = worker.jobs[job["id"]]
        self.assertEqual(loaded["status"], "failed")
        self.assertEqual(loaded["error"], "Translation of FR failed")
        self.assertEqual(loaded["quality"]["en"]["score"], 91)
        self.assertEqual(loaded["review_task"]["status"], "failed")
        self.assertIn("restart", loaded["review_task"]["stage"])

    def test_single_worker_dispatches_generation_then_review_and_keeps_failures_separate(self):
        job = self.job(status="failed")
        worker.queue_review(job["id"], {})
        order = []
        fake_queue = Mock()
        fake_queue.get.side_effect = ["generation-id", {"kind": "review", "id": job["id"]}, StopIteration]
        def generation(identifier):
            order.append(("generation", identifier))
        def review(identifier):
            order.append(("review", identifier))
            raise RuntimeError("Review inputs disappeared")
        with (patch.object(worker, "job_queue", fake_queue), patch.object(worker, "run_job", side_effect=generation),
              patch.object(worker, "run_review_task", side_effect=review), self.assertLogs(level="ERROR")):
            with self.assertRaises(StopIteration):
                worker.worker_loop()
        self.assertEqual(order, [("generation", "generation-id"), ("review", job["id"])])
        self.assertEqual(fake_queue.task_done.call_count, 2)
        self.assertEqual(job["status"], "failed")
        self.assertEqual(job["error"], "Translation of FR failed")
        self.assertEqual(job["review_task"]["status"], "failed")
        self.assertEqual(job["review_task"]["error"], "Review inputs disappeared")

    def test_restart_marks_partial_reviews_unavailable_without_scoring_them(self):
        for index, status in enumerate(("completed", "running"), 1):
            with self.subTest(status=status):
                job = self.job(f"{index:012x}", status=status)
                job["quality"]["en"] = {"status": "running", "reviewed_cues": 8, "total_cues": 9}
                if status == "completed":
                    worker.queue_review(job["id"], {})
                else:
                    with worker.jobs_lock:
                        worker.persist_locked()
                worker.jobs.clear()
                worker.load_jobs()
                loaded = worker.jobs[job["id"]]
                self.assertEqual(loaded["status"], status if status == "completed" else "failed")
                report = loaded["quality"]["en"]
                self.assertEqual(report["status"], "unavailable")
                self.assertEqual(report["reviewed_cues"], 8)
                self.assertIn("restarted", report["error"])
                self.assertNotIn("score", report)

    def test_http_review_routes_return_conflict_and_bulk_results(self):
        job = self.job()
        def post(path, payload):
            handler = worker.Handler.__new__(worker.Handler)
            body = json.dumps(payload).encode()
            handler.path, handler.headers, handler.rfile = path, {"Content-Length": str(len(body))}, io.BytesIO(body)
            result = []
            handler.send_json = lambda value, status=200: result.append((value, status))
            handler.do_POST()
            return result[0]
        response, status = post(f"/api/jobs/{job['id']}/review", {})
        self.assertEqual(status, 202)
        self.assertEqual(response["review_task"]["status"], "queued")
        self.assertEqual(post(f"/api/jobs/{job['id']}/review", {})[1], 409)
        self.assertEqual(post("/api/jobs/ffffffffffff/review", {})[1], 404)
        response, status = post("/api/reviews/missing", {})
        self.assertEqual(status, 202)
        self.assertEqual(response["queued"], [])
        self.assertEqual(response["skipped"][0]["id"], job["id"])


if __name__ == "__main__":
    unittest.main()
