"""Exercise the real event stream using isolated data and local HTTP sockets."""

import http.client
import json
import queue
import socket
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

import worker


class EventClient:
    def __init__(self, address, headers=None, path="/api/jobs/events"):
        self.connection = http.client.HTTPConnection(*address, timeout=2)
        self.connection.connect()
        self.socket = self.connection.sock
        self.connection.request("GET", path, headers=headers or {})
        self.response = self.connection.getresponse()

    def frame(self):
        fields = {}
        comments = []
        while True:
            line = self.response.readline()
            if not line:
                raise EOFError("The event stream closed before a complete frame")
            line = line.decode("utf-8").rstrip("\r\n")
            if not line:
                return fields, comments
            if line.startswith(":"):
                comments.append(line[1:].strip())
            else:
                name, _, value = line.partition(":")
                fields[name] = value.lstrip(" ")

    def jobs(self):
        for _ in range(20):
            fields, _ = self.frame()
            if fields.get("event") == "jobs":
                return fields["id"], json.loads(fields["data"])
        raise AssertionError("No jobs event was received")

    def close(self):
        try:
            self.socket.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.response.close()
        self.connection.close()


class JobsEventTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.data = self.root / "data"
        self.library = self.root / "library"
        self.library.mkdir()
        (self.library / "film.mkv").write_bytes(b"isolated media fixture")
        self.clients = []
        self.stream_condition = threading.Condition()
        self.active_streams = 0
        self.patches = [
            patch.object(worker, "DATA_ROOT", self.data),
            patch.object(worker, "JOBS_FILE", self.data / "jobs.json"),
            patch.object(worker, "OUTPUT_ROOT", self.root / "output"),
            patch.object(worker, "LIBRARY_ROOT", self.library),
            patch.object(worker, "jobs", {}),
            patch.object(worker, "jobs_revision", 0),
            patch.object(worker, "jobs_stream_epoch", "test-epoch"),
            patch.object(worker, "job_queue", queue.Queue()),
            patch.object(worker, "SSE_HEARTBEAT_SECONDS", .05),
            patch.object(worker, "SSE_WRITE_TIMEOUT_SECONDS", 1),
        ]
        for item in self.patches:
            item.start()

        owner = self

        class TrackingHandler(worker.Handler):
            def log_message(self, *args):
                pass

            def stream_jobs(self, view=None):
                with owner.stream_condition:
                    owner.active_streams += 1
                    owner.stream_condition.notify_all()
                try:
                    super().stream_jobs(view)
                finally:
                    with owner.stream_condition:
                        owner.active_streams -= 1
                        owner.stream_condition.notify_all()

        self.handler = TrackingHandler
        self.server = worker.ThreadingHTTPServer(("127.0.0.1", 0), self.handler)
        self.server_thread = threading.Thread(
            target=self.server.serve_forever, kwargs={"poll_interval": .02}, daemon=True)
        self.server_thread.start()
        self.address = self.server.server_address
        self.seed()

    def tearDown(self):
        for client in self.clients:
            client.close()
        with self.stream_condition:
            closed = self.stream_condition.wait_for(lambda: self.active_streams == 0, timeout=2)
        self.server.shutdown()
        self.server.server_close()
        self.server_thread.join(timeout=2)
        for item in reversed(self.patches):
            item.stop()
        self.directory.cleanup()
        self.assertTrue(closed, "Disconnected event stream threads did not finish")

    def seed(self, identifier="012345abcdef", **changes):
        job = {
            "id": identifier, "path": "film.mkv", "filename": "Film 日本語.mkv",
            "source_language": "ja", "targets": ["en"], "transcript": True,
            "audio_stream_index": 1, "model": "local-translator", "review_enabled": False,
            "status": "queued", "stage": "Waiting", "progress": 0,
            "outputs": {}, "quality": {}, "reused": [], "error": None,
            "created_at": "2026-10-06T10:00:00+00:00", "updated_at": worker.now(),
            **changes,
        }
        with worker.jobs_lock:
            worker.jobs[identifier] = job
        return job

    def connect(self, headers=None, path="/api/jobs/events"):
        client = EventClient(self.address, headers, path)
        self.clients.append(client)
        return client

    def request(self, method, path, payload=None):
        connection = http.client.HTTPConnection(*self.address, timeout=2)
        try:
            body = json.dumps(payload).encode() if payload is not None else None
            headers = {"Content-Type": "application/json"} if body is not None else {}
            connection.request(method, path, body=body, headers=headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def test_initial_event_has_stream_headers_id_and_public_utf8_snapshot(self):
        client = self.connect()
        self.assertEqual(client.response.status, 200)
        self.assertEqual(client.response.getheader("Content-Type"), "text/event-stream; charset=utf-8")
        self.assertEqual(client.response.getheader("Cache-Control"), "no-store")
        self.assertEqual(client.response.getheader("X-Accel-Buffering"), "no")
        self.assertIsNone(client.response.getheader("Content-Length"))
        fields, comments = client.frame()
        self.assertEqual(fields, {"retry": "3000"})
        self.assertEqual(comments, [])
        event_id, jobs = client.jobs()
        self.assertEqual(event_id, "test-epoch:0")
        self.assertEqual(jobs[0]["filename"], "Film 日本語.mkv")
        self.assertIn("steps", jobs[0])
        self.assertIn("review_available", jobs[0])
        self.assertTrue(jobs[0]["can_cancel"])
        self.assertEqual(jobs, self.request("GET", "/api/jobs")[1])

    def test_persisted_progress_is_broadcast_to_all_connected_clients(self):
        clients = [self.connect(), self.connect()]
        for client in clients:
            client.jobs()
        worker.set_job("012345abcdef", status="running", stage="Transcribing", progress=37)
        for client in clients:
            event_id, jobs = client.jobs()
            self.assertEqual(event_id, "test-epoch:1")
            self.assertEqual((jobs[0]["status"], jobs[0]["progress"]), ("running", 37))
        persisted = json.loads(worker.JOBS_FILE.read_text(encoding="utf-8"))
        self.assertEqual(persisted[0]["progress"], 37)

    def test_idle_stream_sends_comments_without_duplicate_job_snapshots(self):
        client = self.connect()
        client.jobs()
        for _ in range(3):
            fields, comments = client.frame()
            self.assertEqual(fields, {})
            self.assertEqual(comments, ["heartbeat"])
        self.assertEqual(worker.jobs_revision, 0)

    def test_reconnect_always_gets_current_snapshot_even_with_last_event_id(self):
        first = self.connect()
        previous_id, _ = first.jobs()
        first.close()
        worker.set_job("012345abcdef", progress=61)
        resumed = self.connect({"Last-Event-ID": previous_id})
        current_id, jobs = resumed.jobs()
        self.assertEqual((current_id, jobs[0]["progress"]), ("test-epoch:1", 61))
        # A current ID, or an ID from an earlier process epoch, still gets fresh state.
        for last_id in (current_id, "previous-process:900"):
            self.assertEqual(self.connect({"Last-Event-ID": last_id}).jobs(), (current_id, jobs))

    def test_get_and_job_creation_remain_available_while_event_stream_is_open(self):
        client = self.connect()
        client.jobs()
        status, jobs = self.request("GET", "/api/jobs")
        self.assertEqual((status, len(jobs)), (200, 1))
        with patch.object(worker, "probe_media", return_value={"tracks": [{"index": 1}], "duration": 2}):
            status, created = self.request("POST", "/api/jobs", {
                "path": "film.mkv", "audio_stream_index": 1, "source_language": "ja",
                "transcript": True, "targets": ["en"], "review_enabled": False,
            })
        self.assertEqual(status, 202)
        event_id, snapshot = client.jobs()
        self.assertEqual(event_id, "test-epoch:1")
        self.assertIn(created["id"], [job["id"] for job in snapshot])
        queued = worker.job_queue.get_nowait()
        self.assertEqual(queued["id"], created["id"])
        self.assertEqual(self.request("GET", f"/api/jobs/{created['id']}")[0], 200)

    def test_saved_output_and_review_report_publish_updates(self):
        client = self.connect()
        client.jobs()
        cues = [{"start": 0, "end": 1.5, "text": "Hello world"}]
        worker.write_output("012345abcdef", "en", cues)
        _, jobs = client.jobs()
        self.assertEqual(jobs[0]["outputs"]["en"]["name"], "film.en.srt")
        worker.set_quality("012345abcdef", "en", {
            "status": "completed", "score": 93, "reviewed_cues": 1, "total_cues": 1,
        })
        event_id, jobs = client.jobs()
        self.assertEqual(event_id, "test-epoch:2")
        self.assertEqual(jobs[0]["quality"]["en"]["score"], 93)

    def test_latest_hundred_order_matches_existing_jobs_api(self):
        for number in range(105):
            self.seed(f"{number:012x}", created_at=f"2026-10-06T10:{number // 60:02}:{number % 60:02}+00:00")
        _, snapshot = self.connect().jobs()
        self.assertEqual(len(snapshot), 100)
        self.assertEqual(snapshot[0]["id"], f"{104:012x}")
        self.assertEqual(snapshot, self.request("GET", "/api/jobs")[1])

    def test_failed_persistence_does_not_publish_a_revision(self):
        client = self.connect()
        client.jobs()
        with patch.object(Path, "replace", side_effect=OSError("disk is full")):
            with self.assertRaisesRegex(OSError, "disk is full"):
                worker.set_job("012345abcdef", progress=12)
        self.assertEqual(worker.jobs_revision, 0)
        self.assertFalse(worker.JOBS_FILE.exists())
        fields, comments = client.frame()
        self.assertEqual((fields, comments), ({}, ["heartbeat"]))
        worker.set_job("012345abcdef", progress=13)
        self.assertEqual(client.jobs()[1][0]["progress"], 13)

    def test_disconnect_ends_stream_without_server_error_response(self):
        with patch.object(worker.logging, "exception") as logged:
            client = self.connect()
            client.jobs()
            client.close()
            with self.stream_condition:
                self.assertTrue(self.stream_condition.wait_for(lambda: self.active_streams == 0, timeout=2))
            logged.assert_not_called()
        self.assertEqual(self.request("GET", "/api/jobs")[0], 200)

    def assert_update_does_not_block(self):
        completed = threading.Event()
        errors = []

        def update():
            try:
                worker.set_job("012345abcdef", progress=42)
            except Exception as exc:
                errors.append(exc)
            finally:
                completed.set()

        thread = threading.Thread(target=update, daemon=True)
        thread.start()
        self.assertTrue(completed.wait(.5), "A slow event client held jobs_lock and blocked processing")
        thread.join(timeout=1)
        self.assertEqual(errors, [])

    def test_snapshot_enrichment_does_not_hold_jobs_lock(self):
        entered = threading.Event()
        released = threading.Event()
        original = worker.public_job

        def slow_public_job(job):
            entered.set()
            released.wait(2)
            return original(job)

        with patch.object(worker, "public_job", side_effect=slow_public_job):
            client = self.connect()
            try:
                self.assertTrue(entered.wait(1))
                self.assert_update_does_not_block()
            finally:
                released.set()
            self.assertEqual(client.jobs()[1][0]["progress"], 0)
            self.assertEqual(client.jobs()[1][0]["progress"], 42)

    def test_slow_socket_write_does_not_hold_jobs_lock(self):
        entered = threading.Event()
        released = threading.Event()
        original_end_headers = self.handler.end_headers

        class BlockingWriter:
            def __init__(self, original):
                self.original = original

            def write(self, value):
                if b"event: jobs" in value:
                    entered.set()
                    released.wait(2)
                return self.original.write(value)

            def __getattr__(self, name):
                return getattr(self.original, name)

        def wrap_after_headers(handler):
            original_end_headers(handler)
            if handler.path == "/api/jobs/events":
                handler.wfile = BlockingWriter(handler.wfile)

        with patch.object(self.handler, "end_headers", wrap_after_headers):
            client = self.connect()
            try:
                self.assertTrue(entered.wait(1))
                self.assert_update_does_not_block()
            finally:
                released.set()
            self.assertEqual(client.jobs()[1][0]["progress"], 0)
            self.assertEqual(client.jobs()[1][0]["progress"], 42)

    def test_pagination_reads_full_history_beyond_legacy_latest_hundred(self):
        worker.jobs.clear()
        for number in range(105):
            self.seed(f"{number:012x}", created_at=f"2026-10-06T10:{number // 60:02}:{number % 60:02}+00:00")
        status, first = self.request("GET", "/api/jobs?page=1&page_size=5&status=all")
        self.assertEqual(status, 200)
        self.assertEqual({key: value for key, value in first.items() if key != "jobs"}, {
            "page": 1, "page_size": 5, "total": 105, "total_all": 105, "page_count": 21,
        })
        self.assertEqual([job["id"] for job in first["jobs"]], [f"{number:012x}" for number in range(104, 99, -1)])
        _, last = self.request("GET", "/api/jobs?page=21&page_size=5&status=all")
        self.assertEqual([job["id"] for job in last["jobs"]], [f"{number:012x}" for number in range(4, -1, -1)])
        self.assertEqual(len(self.request("GET", "/api/jobs")[1]), 100)
        self.assertEqual(self.connect(path="/api/jobs/events?page=21&page_size=5&status=all").jobs()[1], last)

    def test_filters_include_independent_work_before_paginating(self):
        worker.jobs.clear()
        fixtures = [
            {"status": "completed"},
            {"status": "failed", "review_task": {"status": "running"}},
            {"status": "completed", "step_task": {"status": "cancelling"}},
            {"status": "running"},
            {"status": "queued"},
            {"status": "cancelled"},
            {"status": "failed", "step_task": {"status": "cancelled"}},
            {"status": "completed", "review_task": {"status": "queued"}},
            {"status": "failed", "review_task": {"status": "failed"}},
        ]
        for number, fixture in enumerate(fixtures, 1):
            self.seed(f"{number:012x}", created_at=f"2026-10-06T10:00:{number:02}+00:00", **fixture)
        expected = {
            "all": list(range(9, 0, -1)), "completed": [8, 3, 1], "failed": [9, 7, 2],
            "running": [4, 3, 2], "queued": [8, 5], "cancelled": [7, 6],
        }
        for status, identifiers in expected.items():
            with self.subTest(status=status):
                code, snapshot = self.request("GET", f"/api/jobs?page=2&page_size=1&status={status}")
                self.assertEqual(code, 200)
                self.assertEqual(snapshot["jobs"][0]["id"], f"{identifiers[1]:012x}")
                self.assertEqual((snapshot["page"], snapshot["total"], snapshot["total_all"], snapshot["page_count"]),
                                 (2, len(identifiers), 9, len(identifiers)))
        _, defaulted = self.request("GET", "/api/jobs?status=running")
        self.assertEqual((defaulted["page"], defaulted["page_size"]), (1, 5))
        self.assertEqual([job["id"] for job in defaulted["jobs"]], [f"{number:012x}" for number in [4, 3, 2]])

    def test_out_of_range_page_clamps_and_empty_filter_has_one_page(self):
        _, clamped = self.request("GET", "/api/jobs?page=999&page_size=5&status=all")
        self.assertEqual((clamped["page"], clamped["page_count"], clamped["total"]), (1, 1, 1))
        status, empty = self.request("GET", "/api/jobs?page=999&page_size=5&status=completed")
        self.assertEqual(status, 200)
        self.assertEqual(empty, {"jobs": [], "page": 1, "page_size": 5, "total": 0, "total_all": 1, "page_count": 1})
        self.assertEqual(self.connect(path="/api/jobs/events?page=999&page_size=5&status=completed").jobs()[1], empty)
        worker.jobs.clear()
        self.assertEqual(self.request("GET", "/api/jobs?page=2")[1], {
            "jobs": [], "page": 1, "page_size": 5, "total": 0, "total_all": 0, "page_count": 1,
        })

    def test_invalid_paging_returns_json_400_before_stream_headers(self):
        queries = ["page=0", "page=-1", "page=1.5", "page=", "page=x", "page=1&page=2",
                   "page_size=0", "page_size=51", "page_size=-1", "page_size=", "page_size=2.5",
                   "page_size=5&page_size=10", "status=unknown", "status=", "status=all&status=queued"]
        for path in ("/api/jobs", "/api/jobs/events"):
            for query in queries:
                with self.subTest(path=path, query=query):
                    status, response = self.request("GET", f"{path}?{query}")
                    self.assertEqual(status, 400)
                    self.assertIsInstance(response.get("error"), str)
        self.assertEqual(self.active_streams, 0)
        self.assertEqual(worker.jobs_revision, 0)

    def test_paged_streams_keep_separate_pages_and_push_changed_counts(self):
        worker.jobs.clear()
        for number in range(6):
            self.seed(f"{number:012x}", created_at=f"2026-10-06T10:00:{number:02}+00:00")
        first = self.connect(path="/api/jobs/events?page=1&page_size=2&status=all")
        second = self.connect(path="/api/jobs/events?page=2&page_size=2&status=all")
        self.assertEqual([job["id"] for job in first.jobs()[1]["jobs"]], [f"{number:012x}" for number in (5, 4)])
        self.assertEqual([job["id"] for job in second.jobs()[1]["jobs"]], [f"{number:012x}" for number in (3, 2)])
        self.seed("000000000006", created_at="2026-10-06T10:00:06+00:00")
        worker.set_job("000000000006", progress=7)
        for client, page, numbers in ((first, 1, (6, 5)), (second, 2, (4, 3))):
            event_id, snapshot = client.jobs()
            self.assertEqual(event_id, "test-epoch:1")
            self.assertEqual((snapshot["page"], snapshot["total"], snapshot["total_all"], snapshot["page_count"]),
                             (page, 7, 7, 4))
            self.assertEqual([job["id"] for job in snapshot["jobs"]], [f"{number:012x}" for number in numbers])
        replay = self.connect({"Last-Event-ID": "test-epoch:1"}, path="/api/jobs/events?page=2&page_size=2&status=all")
        self.assertEqual(replay.jobs()[1], self.request("GET", "/api/jobs?page=2&page_size=2&status=all")[1])

    def test_filtered_stream_clamps_page_when_jobs_leave_selected_status(self):
        worker.jobs.clear()
        for number in range(3):
            self.seed(f"{number:012x}", created_at=f"2026-10-06T10:00:{number:02}+00:00")
        client = self.connect(path="/api/jobs/events?page=3&page_size=1&status=queued")
        self.assertEqual(client.jobs()[1]["jobs"][0]["id"], "000000000000")
        worker.set_job("000000000000", status="completed")
        _, updated = client.jobs()
        self.assertEqual((updated["page"], updated["page_count"], updated["total"], updated["total_all"]), (2, 2, 2, 3))
        self.assertEqual(updated["jobs"][0]["id"], "000000000001")
        for number in (1, 2):
            worker.set_job(f"{number:012x}", status="completed")
            _, updated = client.jobs()
        self.assertEqual((updated["page"], updated["page_count"], updated["total"], updated["jobs"]), (1, 1, 0, []))


if __name__ == "__main__":
    unittest.main()
