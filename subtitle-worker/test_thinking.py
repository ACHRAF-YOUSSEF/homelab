import io
import json
import unittest
from unittest.mock import patch

import worker
import test_review_actions as review_fixtures
import test_step_actions as step_fixtures
from test_review_actions import mock_ratings


class ThinkingTests(unittest.TestCase):
    def setUp(self):
        self.fixture = review_fixtures.ExistingReviewTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)
        self.read_options = worker.model_thinking_options
        self.options = patch.object(worker, "model_thinking_options", return_value={
            "translator": {"can_toggle": True, "effort_on": "high"},
            "reviewer": {"can_toggle": True, "effort_on": "high"},
        })
        self.options.start()
        self.addCleanup(self.options.stop)

    def test_capabilities_use_metadata_and_loaded_instance_aliases(self):
        def model(key, allowed, instance=None):
            return {"type": "llm", "key": key,
                    "loaded_instances": [{"id": instance}] if instance else [],
                    "capabilities": {"reasoning": {"allowed_options": allowed}}}
        data = {"models": [model("gemma", ["off", "on"], "gemma-instance"),
                           model("graded", ["off", "low", "medium", "high"]),
                           model("always", ["on"]), model("plain", [])]}
        with (patch.object(worker, "thinking_metadata", {"expires": 0, "models": {}}),
              patch.object(worker, "open_http", return_value=io.BytesIO(json.dumps(data).encode())) as open_model):
            options = self.read_options()
            self.assertTrue(options["gemma-instance"]["can_toggle"])
            self.assertEqual(options["gemma"]["effort_on"], "high")
            self.assertEqual(options["graded"]["effort_on"], "high")
            self.assertFalse(options["always"]["can_toggle"])
            self.assertFalse(options["plain"]["can_toggle"])
            self.read_options()
            open_model.assert_called_once()

    def test_real_translation_and_review_payloads_control_reasoning(self):
        calls = []
        def response(request, timeout):
            body = json.loads(request.data)
            calls.append(body)
            reply = (mock_ratings("", "", "", self.fixture.cues, [], 0)
                     if "response_format" in body else [{"id": 1, "text": "Hello"}])
            return io.BytesIO(json.dumps({"choices": [{"message": {"content": json.dumps(reply)}}]}).encode())
        with patch.object(worker, "open_http", side_effect=response):
            for thinking, effort in ((False, "none"), (True, "high"), (None, None)):
                worker.request_translation("translator", "ja", "en", self.fixture.cues[:1], thinking=thinking)
                self.assertEqual(calls[-1].get("reasoning_effort"), effort)
                worker.request_quality_review("reviewer", "ja", "en", self.fixture.cues, ["text"] * 9, 0, thinking=thinking)
                self.assertEqual(calls[-1].get("reasoning_effort"), effort)
                self.assertEqual(calls[-1]["response_format"]["type"], "json_schema")

    def test_unsupported_controls_fail_instead_of_silently_ignoring(self):
        for model in ("unknown", "google/translategemma-12b-it"):
            with self.subTest(model=model), self.assertRaises(ValueError):
                worker.apply_thinking({}, model, False)
        payload = {}
        worker.apply_thinking(payload, "unknown", None)
        self.assertEqual(payload, {})
        for invalid in (0, 1, "false", [], {}):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                worker.validate_review_action({"review_thinking": invalid})

    def test_each_mode_has_its_own_cache_but_legacy_keys_stay_valid(self):
        for function, args in (
            (worker.translation_cache_key, ("source", "ja", self.fixture.cues, "en", "translator")),
            (worker.quality_cache_key, ("source", "ja", self.fixture.cues, "en", ["text"] * 9, "reviewer")),
        ):
            with self.subTest(function=function.__name__):
                default = function(*args)
                self.assertEqual(default, function(*args, thinking=None))
                self.assertEqual(len({default, function(*args, thinking=False), function(*args, thinking=True)}), 3)

    def test_saved_review_mode_reaches_every_batch_and_retry(self):
        job = self.fixture.job()
        calls = []
        def review(*args, thinking=None):
            calls.append((args[-1], thinking))
            if args[-1] == 8:
                raise ValueError("Temporary failure")
            return mock_ratings(*args)
        worker.queue_review(job["id"], {"review_model": "reviewer", "review_thinking": True})
        with patch.object(worker, "request_quality_review", side_effect=review):
            self.fixture.run_queued_review(job["id"])
        self.assertEqual(calls, [(0, True), (8, True)])
        worker.retry_step(job["id"], "review:en")
        self.assertTrue(job["review_task"]["thinking"])
        with patch.object(worker, "request_quality_review", side_effect=lambda *args, **kwargs: mock_ratings(*args)) as retry:
            self.fixture.run_queued_review(job["id"])
        self.assertEqual(retry.call_count, 1)
        self.assertEqual(retry.call_args.kwargs, {"thinking": True})
        self.assertTrue(job["quality"]["en"]["thinking"])
        # Changing mode starts fresh review batches without regenerating SRTs.
        saved = {code: worker.Path(output["path"]).read_bytes() for code, output in job["outputs"].items()}
        worker.queue_review(job["id"], {"languages": ["en"], "review_model": "reviewer", "review_thinking": False})
        with patch.object(worker, "request_quality_review", side_effect=lambda *args, **kwargs: mock_ratings(*args)) as reviewer:
            self.fixture.run_queued_review(job["id"])
        self.assertEqual(reviewer.call_count, 2)
        self.assertFalse(job["quality"]["en"]["thinking"])
        self.assertEqual(saved, {code: worker.Path(output["path"]).read_bytes() for code, output in job["outputs"].items()})

    def test_bulk_review_persists_selected_mode(self):
        job = self.fixture.job()
        result = worker.queue_missing_reviews({"review_model": "reviewer", "review_thinking": False})
        self.assertEqual(result["queued"], [job["id"]])
        self.assertIs(job["review_task"]["thinking"], False)

    def test_generation_changes_translation_mode_without_repeating_whisper(self):
        fixture = step_fixtures.StepActionTests()
        fixture.setUp()
        try:
            calls = []
            def translate(*args, thinking=None):
                calls.append(thinking)
                return fixture.translate(*args)
            with (patch.object(worker, "extract_audio", side_effect=fixture.extract),
                  patch.object(worker, "get_speech_model", return_value=fixture.speech) as speech,
                  patch.object(worker, "request_translation", side_effect=translate)):
                for thinking in (False, False, True):
                    fixture.create(translation_thinking=thinking)
                    fixture.drain()
                with patch.object(worker, "request_quality_review", side_effect=lambda *args, **kwargs: mock_ratings(*args)) as reviewer:
                    reviewed = fixture.create(translation_thinking=False, review_enabled=True,
                                              review_model="reviewer", review_thinking=True)
                    fixture.drain()
                self.assertEqual(reviewer.call_count, 4)
                self.assertTrue(all(call.kwargs == {"thinking": True} for call in reviewer.call_args_list))
                self.assertTrue(all(report["thinking"] for report in reviewed["quality"].values()))
                self.assertEqual(speech.call_count, 1)
            self.assertEqual(calls, [False] * 4 + [True] * 4)
            for invalid in (0, 1, "off", []):
                with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                    fixture.create(translation_thinking=invalid)
        finally:
            fixture.tearDown()


if __name__ == "__main__":
    unittest.main()
