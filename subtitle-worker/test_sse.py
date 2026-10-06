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
    def __init__(self, address, headers=None):
        self.connection = http.client.HTTPConnection(*address, timeout=2)
        self.connection.connect()
        self.socket = self.connection.sock
        self.connection.request("GET", "/api/jobs/events", headers=headers or {})
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

            def stream_jobs(self):
                with owner.stream_condition:
                    owner.active_streams += 1
                    owner.stream_condition.notify_all()
                try:
                    super().stream_jobs()
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

    def connect(self, headers=None):
        client = EventClient(self.address, headers)
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


if __name__ == "__main__":
    unittest.main()
