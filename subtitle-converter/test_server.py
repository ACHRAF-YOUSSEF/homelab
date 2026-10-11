"""Focused dashboard tests using loopback HTTP and disposable media paths."""

import base64
from contextlib import redirect_stderr
import http.client
import io
import json
import os
from pathlib import Path
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from converter import Converter
import server
from server import DashboardServer, JobManager, MAX_BODY


PASSWORD = "unit-test-dashboard-password"
WEBHOOK_TOKEN = "unit-test-webhook-token"


class FakeIntegrations:
    def __init__(self, mapped_path=None, webhook_path="remote:/fixture/Example.mkv"):
        self.mapped_path = mapped_path
        self.webhook_path = webhook_path
        self.webhook_payloads = []

    def public_settings(self):
        return {name: {"enabled": False, "api_key": "", "api_key_set": False}
                for name in ("sonarr", "radarr", "jellyfin")} | {"path_mappings": []}

    def update(self, settings):
        return self.public_settings()

    def notify(self, paths):
        return []

    def libraries(self):
        return []

    def test(self, provider):
        return {"ok": True}

    def refresh(self, provider, **kwargs):
        return {"ok": True}

    def webhook_paths(self, provider, payload):
        self.webhook_payloads.append((provider, payload))
        return [self.webhook_path] if payload.get("eventType") != "Test" else []

    def map_path(self, provider, path):
        return str(self.mapped_path) if self.mapped_path is not None else None


class DashboardServerTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / "library"
        self.root.mkdir()
        self.media = self.root / "Example.mkv"
        self.media.write_bytes(b"disposable video fixture")
        self.outside = self.base / "outside.mkv"
        self.outside.write_bytes(b"outside fixture")
        self.converter = Converter([self.root], self.base / "state.json", min_age=0)
        self.integrations = FakeIntegrations(self.media)
        self.manager = JobManager(self.converter, self.integrations, auto_scan=False)
        self.server = DashboardServer(
            ("127.0.0.1", 0), self.manager, PASSWORD,
            allowed_hosts=["127.0.0.1"], webhook_token=WEBHOOK_TOKEN,
        )
        self.server_thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.server_thread.start()
        self.addCleanup(self._close_server)

    def _close_manager(self, manager):
        if manager.thread.ident is None:
            manager.stop.set()
            manager.wake.set()
            manager.start()
            manager.thread.join(timeout=3)
        else:
            manager.close()

    def _close_server(self):
        self.server.shutdown()
        self.server.server_close()
        self.server_thread.join(timeout=3)
        self._close_manager(self.manager)

    @property
    def host(self):
        return f"127.0.0.1:{self.server.server_address[1]}"

    def request(self, method, path, *, body=None, auth=True, csrf=None, headers=None):
        request_headers = {"Host": self.host}
        if auth:
            token = base64.b64encode(f"admin:{PASSWORD}".encode()).decode()
            request_headers["Authorization"] = "Basic " + token
        if body is not None:
            request_headers["Content-Type"] = "application/json"
        if csrf is not None:
            request_headers["X-CSRF-Token"] = csrf
        request_headers.update(headers or {})
        encoded = json.dumps(body).encode() if body is not None else None
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_address[1], timeout=3)
        try:
            connection.request(method, path, body=encoded, headers=request_headers)
            response = connection.getresponse()
            return response.status, response.read(), dict(response.getheaders())
        finally:
            connection.close()

    def test_authentication_csrf_origin_host_and_body_limit(self):
        status, _, headers = self.request("GET", "/api/status", auth=False)
        self.assertEqual(status, 401)
        self.assertIn("WWW-Authenticate", headers)

        status, _, headers = self.request("GET", "/health", auth=False)
        self.assertEqual(status, 200)
        self.assertEqual(headers.get("X-Frame-Options"), "DENY")
        self.assertEqual(headers.get("Cache-Control"), "no-store")

        status, _, _ = self.request("GET", "/api/status", headers={"Host": "attacker.invalid"})
        self.assertEqual(status, 403)
        status, _, _ = self.request("GET", "/api/status", headers={"Origin": "https://attacker.invalid"})
        self.assertEqual(status, 403)

        status, body, _ = self.request("GET", "/api/session")
        self.assertEqual(status, 200)
        csrf = json.loads(body)["csrf_token"]
        status, _, _ = self.request("POST", "/api/jobs", body={"kind": "scan"})
        self.assertEqual(status, 403)
        status, body, _ = self.request("POST", "/api/jobs", body={"kind": "scan"}, csrf=csrf)
        self.assertEqual(status, 202)
        self.assertEqual(json.loads(body)["job"]["kind"], "scan")

        oversize = json.dumps({"data": "x" * MAX_BODY}).encode()
        self.assertGreater(len(oversize), MAX_BODY)
        status, _, _ = self.request("POST", "/api/jobs", body={"data": "x" * MAX_BODY}, csrf=csrf)
        self.assertEqual(status, 400)

    def test_dashboard_assets_match_document_paths_and_require_authentication(self):
        status, content, headers = self.request("GET", "/")
        self.assertEqual(status, 200)
        self.assertIn(b"/static/app.js", content)
        self.assertIn("script-src 'self'", headers["Content-Security-Policy"])
        for path, content_type in [("/static/app.js", "text/javascript"),
                                   ("/static/style.css", "text/css"),
                                   ("/static/icon.svg", "image/svg+xml")]:
            status, content, headers = self.request("GET", path)
            self.assertEqual(status, 200)
            self.assertTrue(content)
            self.assertTrue(headers["Content-Type"].startswith(content_type))
            self.assertEqual(self.request("GET", path, auth=False)[0], 401)
        self.assertEqual(self.request("GET", "/static/../converter.py")[0], 404)

    def test_hook_token_and_mapped_paths_are_constrained_to_library(self):
        payload = {"eventType": "Download", "path": "fixture"}
        status, _, _ = self.request("POST", "/hooks/sonarr", body=payload, auth=False)
        self.assertEqual(status, 401)
        status, _, _ = self.request(
            "POST", "/hooks/sonarr", body=payload, auth=False,
            headers={"X-Webhook-Token": "incorrect-token"},
        )
        self.assertEqual(status, 401)

        status, body, _ = self.request(
            "POST", "/hooks/sonarr", body={"eventType": "Test"}, auth=False,
            headers={"X-Webhook-Token": WEBHOOK_TOKEN},
        )
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), {"ok": True})
        self.assertEqual(len(self.manager.queue), 0)

        status, body, _ = self.request(
            "POST", "/hooks/sonarr", body=payload, auth=False,
            headers={"X-Webhook-Token": WEBHOOK_TOKEN},
        )
        self.assertEqual(status, 202)
        self.assertEqual(json.loads(body)["job"]["paths"], [str(self.media)])
        self.assertEqual(self.integrations.webhook_payloads[-1][0], "sonarr")

        self.integrations.mapped_path = self.outside
        status, _, _ = self.request(
            "POST", "/hooks/sonarr", body=payload, auth=False,
            headers={"X-Webhook-Token": WEBHOOK_TOKEN},
        )
        self.assertEqual(status, 400)
        self.assertEqual(len(self.manager.queue), 1)

        # If no mapping matches, same-root absolute paths are still validated
        # by enqueue before they can become conversion jobs.
        self.integrations.mapped_path = None
        self.integrations.webhook_path = str(self.media)
        status, body, _ = self.request(
            "POST", "/hooks/sonarr", body=payload, auth=False,
            headers={"X-Webhook-Token": WEBHOOK_TOKEN},
        )
        self.assertEqual(status, 202)
        self.assertEqual(json.loads(body)["job"]["paths"], [str(self.media)])

        self.integrations.webhook_path = str(self.outside)
        status, _, _ = self.request(
            "POST", "/hooks/sonarr", body=payload, auth=False,
            headers={"X-Webhook-Token": WEBHOOK_TOKEN},
        )
        self.assertEqual(status, 400)
        self.assertEqual(len(self.manager.queue), 2)

    def test_media_path_must_be_supported_inside_root_and_not_a_symlink(self):
        self.assertEqual(self.manager.safe_path(str(self.media)), self.media)
        with self.assertRaises(ValueError):
            self.manager.safe_path(str(self.outside))
        with self.assertRaises(ValueError):
            self.manager.safe_path(str(self.root / ".." / "outside.mkv"))

        link = self.root / "linked.mkv"
        try:
            link.symlink_to(self.outside)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlinks unavailable: {type(error).__name__}")
        with self.assertRaises(ValueError):
            self.manager.safe_path(str(link))

    def test_queue_capacity_snapshot_and_cancel_states(self):
        manager = JobManager(self.converter, self.integrations, auto_scan=False)
        self.addCleanup(self._close_manager, manager)
        jobs = [manager.enqueue({"kind": "scan"}) for _ in range(32)]
        self.assertIsNone(manager.enqueue({"kind": "scan"}, origin="schedule"))
        with self.assertRaises(ValueError):
            manager.enqueue({"kind": "scan"})

        snapshot = manager.status()
        snapshot["queue"][0]["kind"] = "changed in copy"
        self.assertEqual(manager.queue[0]["kind"], "scan")
        self.assertEqual(manager.cancel(jobs[0]["id"]), {"status": "cancelled"})

        manager.running = {"id": "running-fixture"}
        manager.cancel_event = threading.Event()
        self.assertEqual(manager.cancel("running-fixture"), {"status": "cancelling"})
        self.assertTrue(manager.cancel_event.is_set())

    def test_worker_serializes_scans_and_cancels_running_job(self):
        entered_first = threading.Event()
        release_first = threading.Event()
        entered_second = threading.Event()
        state_lock = threading.Lock()
        state = {"active": 0, "maximum": 0, "calls": 0}

        def blocking_scan(paths=None, *, retry=False):
            with state_lock:
                state["calls"] += 1
                call = state["calls"]
                state["active"] += 1
                state["maximum"] = max(state["maximum"], state["active"])
            if call == 1:
                entered_first.set()
                release_first.wait(timeout=5)
            else:
                entered_second.set()
            with state_lock:
                state["active"] -= 1
            return {"converted": 0}

        self.converter.scan = blocking_scan
        manager = JobManager(self.converter, self.integrations, auto_scan=False)
        manager.start()
        try:
            first = manager.enqueue({"kind": "scan"})
            self.assertTrue(entered_first.wait(timeout=2))
            second = manager.enqueue({"kind": "scan"})
            self.assertEqual([item["id"] for item in manager.queue], [second["id"]])
            self.assertEqual(manager.cancel(first["id"]), {"status": "cancelling"})
            self.assertTrue(manager.cancel_event.is_set())
            release_first.set()
            self.assertTrue(entered_second.wait(timeout=2))
            deadline = time.monotonic() + 2
            while len(manager.history) < 2 and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertEqual(state["maximum"], 1)
            self.assertEqual([job["status"] for job in manager.history], ["cancelled", "completed"])
        finally:
            release_first.set()
            self._close_manager(manager)

    def test_worker_continues_after_unexpected_job_exception(self):
        first_started = threading.Event()
        release_first = threading.Event()
        state = {"calls": 0}
        state_lock = threading.Lock()

        def scan_then_raise(paths=None, *, retry=False):
            with state_lock:
                state["calls"] += 1
                call = state["calls"]
            if call == 1:
                first_started.set()
                release_first.wait(timeout=5)
                raise AssertionError("fixture-only unexpected exception")
            return {"converted": 0}

        self.converter.scan = scan_then_raise
        manager = JobManager(self.converter, self.integrations, auto_scan=False)
        manager.start()
        try:
            first = manager.enqueue({"kind": "scan"})
            self.assertTrue(first_started.wait(timeout=2))
            manager.enqueue({"kind": "scan"})
            release_first.set()
            deadline = time.monotonic() + 3
            while len(manager.history) < 2 and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertEqual(len(manager.history), 2)
            self.assertEqual(manager.history[0]["id"], first["id"])
            self.assertEqual(manager.history[0]["status"], "failed")
            self.assertEqual(manager.history[0]["error"], "AssertionError")
            self.assertEqual(manager.history[1]["status"], "completed")
            self.assertEqual(state["calls"], 2)
        finally:
            release_first.set()
            self._close_manager(manager)


class DashboardStartupConfigurationTest(unittest.TestCase):
    """Startup failures are exercised with disposable state, never deployment data."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.data = self.base / "data"
        self.environment = {
            "CONVERTER_UI_PASSWORD": "fixture-startup-password",
            "CONVERTER_SCAN_INTERVAL": "300",
            "CONVERTER_MIN_AGE": "120",
            "CONVERTER_TIMEOUT": "7200",
            "CONVERTER_RETRY_INTERVAL": "3600",
            "CONVERTER_OCR_LANGUAGE": "eng",
        }

    def assert_startup_failure(self, expected_message, *, extra_environment=None, missing_tool=False,
                               data_path=None, integrations_error=None):
        environment = dict(self.environment)
        environment.update(extra_environment or {})
        errors = io.StringIO()
        patches = [patch.dict(os.environ, environment, clear=False),
                   patch.object(server.shutil, "which", return_value=None if missing_tool else "/fixture/tool")]
        if integrations_error is not None:
            patches.append(patch.object(server, "Integrations", side_effect=integrations_error))
        for item in patches:
            item.start()
        with redirect_stderr(errors):
            try:
                with self.assertRaises(SystemExit) as raised:
                    server.main(["--data", str(data_path or self.data), "--no-auto-scan"])
            finally:
                for item in reversed(patches):
                    item.stop()
        self.assertEqual(raised.exception.code, 2)
        output = errors.getvalue()
        self.assertIn(expected_message, output)
        for sentinel in ["fixture-startup-password", "fixture-secret-sentinel",
                         "private-path-sentinel", "environment-value-sentinel",
                         str(data_path or self.data)]:
            self.assertNotIn(sentinel, output)

    def test_invalid_or_unparseable_interval_fails_before_server_start(self):
        for name, value in [("CONVERTER_SCAN_INTERVAL", "not-an-integer"),
                            ("CONVERTER_MIN_AGE", "-1"),
                            ("CONVERTER_TIMEOUT", "0"),
                            ("CONVERTER_RETRY_INTERVAL", "0")]:
            with self.subTest(setting=name, value=value):
                self.assert_startup_failure(f"{name} must be an integer", extra_environment={name: value})

    def test_invalid_password_and_ocr_fallback_fail_before_server_start(self):
        for name, value, expected in [("CONVERTER_UI_PASSWORD", "short", "CONVERTER_UI_PASSWORD must have 12–256 printable characters"),
                                      ("CONVERTER_UI_PASSWORD", "fixture-secret-sentinel\nwith-control", "CONVERTER_UI_PASSWORD must have 12–256 printable characters"),
                                      ("CONVERTER_OCR_LANGUAGE", "environment-value-sentinel", "CONVERTER_OCR_LANGUAGE must contain Tesseract language codes")]:
            with self.subTest(setting=name):
                self.assert_startup_failure(expected, extra_environment={name: value})

    def test_missing_required_tool_fails_before_server_start(self):
        self.assert_startup_failure("Required conversion tools were not found: seconv, ffprobe, mkvmerge",
                                    missing_tool=True)

    def test_unwritable_data_path_fails_as_configuration_error(self):
        blocker = self.base / "not-a-directory"
        blocker.write_text("fixture", encoding="utf-8")
        path = blocker / "private-path-sentinel"
        reason = ("a required path already exists; check for a file where a directory is required"
                  if os.name == "nt" else "a required directory path points to a file")
        self.assert_startup_failure(
            f"Cannot initialize dashboard preferences and integration settings: {reason}",
            data_path=path,
        )

    def test_invalid_saved_password_is_rejected(self):
        self.data.mkdir()
        (self.data / "ui-password").write_text("too-short\n", encoding="utf-8")
        self.assert_startup_failure("Saved dashboard password must have 12–256 printable characters",
                                    extra_environment={"CONVERTER_UI_PASSWORD": ""})

    def test_symlinked_saved_password_is_rejected(self):
        self.data.mkdir()
        target = self.base / "fixture-password-target"
        target.write_text("fixture-only-password-long-enough\n", encoding="utf-8")
        try:
            (self.data / "ui-password").symlink_to(target)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlinks unavailable: {type(error).__name__}")
        self.assert_startup_failure("Saved dashboard password file must not be a symlink",
                                    extra_environment={"CONVERTER_UI_PASSWORD": ""})

    def test_unexpected_oserror_reports_phase_and_static_reason_only(self):
        error = OSError(13, "fixture-secret-sentinel", "/tmp/private-path-sentinel")
        self.assert_startup_failure(
            "Cannot initialize dashboard preferences and integration settings: permission denied",
            integrations_error=error,
        )

    def test_unexpected_valueerror_reports_phase_without_exception_text(self):
        error = ValueError("environment-value-sentinel /private-path-sentinel fixture-secret-sentinel")
        self.assert_startup_failure(
            "Cannot initialize dashboard preferences and integration settings: invalid configuration",
            integrations_error=error,
        )

if __name__ == "__main__":
    unittest.main()
