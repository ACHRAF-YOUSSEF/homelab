"""Authenticated local dashboard and serialized subtitle conversion jobs."""

from __future__ import annotations

import argparse
import base64
from collections import deque
import errno
import hmac
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import logging
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import subprocess
import tempfile
import threading
import time
from urllib.parse import urlsplit

from converter import Converter, SIDECARS, VIDEOS, candidates
from integrations import Integrations
from http_client import HTTPError

LOG = logging.getLogger("subtitle-converter")
WEB = Path(__file__).parent / "web"
MAX_BODY = 65536
MAX_INVENTORY = 2000


class StartupError(ValueError):
    """A controlled startup diagnostic containing no configuration values."""


def interval_setting(name, default, minimum):
    message = f"{name} must be an integer of at least {minimum} seconds"
    try:
        value = int(os.environ.get(name, str(default)))
    except ValueError:
        raise StartupError(message) from None
    if value < minimum:
        raise StartupError(message)
    return value


def startup_os_error(phase, error):
    """Do not expose exception text, private paths or configuration values."""
    reasons = {
        errno.EACCES: "permission denied; check the process user and state-directory permissions (container UID/GID 1000)",
        errno.EPERM: "operation not permitted; check permissions for the process user",
        errno.EROFS: "filesystem is read-only; state storage must be writable",
        errno.ENOSPC: "storage is full",
        errno.ENOTDIR: "a required directory path points to a file",
        errno.EEXIST: "a required path already exists; check for a file where a directory is required",
        errno.EISDIR: "a required state-file path points to a directory",
        errno.ENOENT: "a required file or directory is missing",
        errno.EADDRINUSE: "listener port is already in use; check --port",
        errno.EADDRNOTAVAIL: "listener address is unavailable; check --host",
    }
    return f"Cannot initialize {phase}: {reasons.get(error.errno, 'filesystem or network operation failed')}"


def write_private(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                     prefix="dashboard-", delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(value, stream)
        stream.flush()
        os.fsync(stream.fileno())
    try:
        temporary.chmod(0o600)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def ui_password(data_dir):
    configured = os.environ.get("CONVERTER_UI_PASSWORD", "")
    if configured:
        if not 12 <= len(configured) <= 256 or not configured.isprintable():
            raise StartupError("CONVERTER_UI_PASSWORD must have 12–256 printable characters; set a valid private password or leave it blank to generate one")
        return configured
    path = data_dir / "ui-password"
    if path.exists():
        if path.is_symlink():
            raise StartupError("Saved dashboard password file must not be a symlink")
        password = path.read_text(encoding="utf-8").strip()
        if not 12 <= len(password) <= 256 or not password.isprintable():
            raise StartupError("Saved dashboard password must have 12–256 printable characters; replace the password file or set CONVERTER_UI_PASSWORD privately")
        return password
    data_dir.mkdir(parents=True, exist_ok=True)
    password = secrets.token_urlsafe(32)
    with path.open("x", encoding="utf-8") as stream:
        path.chmod(0o600)
        stream.write(password + "\n")
    LOG.info("Dashboard password generated; retrieve /data/ui-password inside the container")
    return password


class ActivityLog(logging.Handler):
    def __init__(self, manager):
        super().__init__()
        self.manager = manager

    def emit(self, record):
        with self.manager.lock:
            self.manager.logs.append({"time": record.created, "level": record.levelname,
                                      "message": record.getMessage()[:2000]})


class JobManager:
    def __init__(self, converter, integrations, *, interval=300, auto_scan=True):
        self.converter = converter
        self.integrations = integrations
        self.interval = interval
        self.auto_scan = auto_scan
        self.lock = threading.RLock()
        self.wake = threading.Event()
        self.stop = threading.Event()
        self.queue = deque()
        self.history = deque(maxlen=100)
        self.logs = deque(maxlen=200)
        self.running = None
        self.cancel_event = None
        self.csrf = secrets.token_urlsafe(32)
        self.preferences = converter.state_path.parent / "preferences.json"
        self.history_path = converter.state_path.parent / "jobs.json"
        self.languages = ["*"]
        try:
            saved = json.loads(self.preferences.read_text(encoding="utf-8"))
            self.languages = converter.normalize_languages(saved.get("languages", ["*"]))
        except FileNotFoundError:
            pass
        except (OSError, ValueError, AttributeError):
            LOG.warning("Dashboard preferences could not be loaded; using all languages")
        try:
            saved = json.loads(self.history_path.read_text(encoding="utf-8"))
            if isinstance(saved, list):
                self.history.extend(entry for entry in saved[-100:] if isinstance(entry, dict))
        except FileNotFoundError:
            pass
        except (OSError, ValueError):
            LOG.warning("Dashboard history could not be loaded")
        self.thread = threading.Thread(target=self._worker, name="subtitle-jobs", daemon=True)

    def roots(self):
        return [{"id": str(index), "path": str(root), "label": root.name or str(root)}
                for index, root in enumerate(self.converter.roots)]

    def safe_path(self, value, roots=None):
        if not isinstance(value, str) or len(value) > 4096 or "\x00" in value:
            raise ValueError("Invalid media path")
        path = Path(value)
        if not path.is_absolute() or ".." in path.parts:
            raise ValueError("Choose an absolute file path inside a configured library")
        for root in roots or self.converter.roots:
            try:
                relative = path.relative_to(root)
            except ValueError:
                continue
            current = root
            if current.is_symlink():
                continue
            for part in relative.parts:
                current /= part
                if current.is_symlink():
                    raise ValueError("Symlink inputs are not supported")
            if not path.resolve().is_relative_to(root.resolve()):
                raise ValueError("Media path is outside its library")
            if not path.is_file() or path.suffix.lower() not in VIDEOS | SIDECARS:
                raise ValueError("Media file is missing or unsupported")
            return path
        raise ValueError("Media path is outside configured libraries")

    def settings(self):
        public = self.integrations.public_settings()
        providers = {name: public[name] for name in ("sonarr", "radarr", "jellyfin")}
        for name in ("sonarr", "radarr"):
            providers[name]["webhook_enabled"] = bool(os.environ.get("CONVERTER_WEBHOOK_TOKEN"))
        with self.lock:
            return {"languages": list(self.languages), "integrations": providers,
                    "path_mappings": public.get("path_mappings", []), "scan_interval": self.interval}

    def update_settings(self, data):
        languages = self.converter.normalize_languages(data.get("languages", self.languages))
        providers = data.get("integrations", {})
        if not isinstance(providers, dict):
            raise ValueError("Invalid integration settings")
        update = dict(providers)
        if "path_mappings" in data:
            update["path_mappings"] = data["path_mappings"]
        self.integrations.update(update)
        with self.lock:
            write_private(self.preferences, {"languages": languages})
            self.languages = languages
        return self.settings()

    def status(self):
        settings = self.settings()
        with self.lock:
            # Round-trip snapshot: request threads never serialize live worker data.
            return json.loads(json.dumps({"running": self.running, "queue": list(self.queue),
                                          "history": list(reversed(self.history)), "logs": list(self.logs),
                                          "roots": self.roots(), "settings": settings}))

    def enqueue(self, data, *, origin="manual"):
        kind = data.get("kind", "scan")
        if kind not in {"scan", "inspect", "extract", "refresh"}:
            raise ValueError("Unknown job type")
        provider = data.get("provider")
        if kind == "refresh" and provider not in {"sonarr", "radarr", "jellyfin"}:
            raise ValueError("Unknown integration")
        root_ids = data.get("root_ids", [])
        if not isinstance(root_ids, list) or any(not isinstance(index, str) or index not in {root["id"] for root in self.roots()} for index in root_ids):
            raise ValueError("Invalid library selection")
        selected_roots = [self.converter.roots[int(index)] for index in root_ids] if root_ids else self.converter.roots
        paths = data.get("paths", [])
        if not isinstance(paths, list) or len(paths) > 256:
            raise ValueError("Select at most 256 files per job")
        paths = [str(self.safe_path(value, selected_roots)) for value in paths]
        if kind == "extract" and not paths:
            raise ValueError("Choose a file to extract")
        languages = self.converter.normalize_languages(data.get("languages", self.languages))
        if not isinstance(data.get("retry", False), bool):
            raise ValueError("Invalid retry option")
        with self.lock:
            if origin == "schedule" and (self.running or self.queue):
                return None
            if len(self.queue) >= 32:
                raise ValueError("Job queue is full")
            job = {"id": secrets.token_hex(8), "kind": kind, "status": "queued", "created": time.time(),
                   "started": None, "finished": None, "current": None, "summary": {}, "error": None,
                   "results": [], "languages": languages, "root_ids": root_ids, "paths": paths,
                   "retry": data.get("retry", False), "origin": origin, "provider": provider,
                   "processed": 0, "total": 0}
            self.queue.append(job)
            self.wake.set()
            return json.loads(json.dumps(job))

    def cancel(self, identifier):
        with self.lock:
            if self.running and self.running["id"] == identifier:
                self.cancel_event.set()
                return {"status": "cancelling"}
            for job in self.queue:
                if job["id"] == identifier:
                    self.queue.remove(job)
                    job.update(status="cancelled", finished=time.time())
                    self.history.append(job)
                    return {"status": "cancelled"}
        raise ValueError("Job is no longer queued or running")

    def start(self):
        self.thread.start()

    def close(self):
        self.stop.set()
        with self.lock:
            if self.cancel_event:
                self.cancel_event.set()
        self.wake.set()
        self.thread.join(timeout=15)

    def _progress(self, event):
        with self.lock:
            if not self.running:
                return
            self.running["current"] = event
            if "summary" in event:
                self.running["summary"] = event["summary"]
            if event["status"] in {"converted", "failed", "pending", "cached"}:
                self.running["processed"] += 1
        if event.get("new_outputs"):
            notifications = self.integrations.notify(event["new_outputs"])
            with self.lock:
                self.running.setdefault("notifications", []).extend(notifications)
                self.running["notifications"] = self.running["notifications"][-200:]

    def _execute(self, job):
        if job["kind"] == "refresh":
            job["results"] = [self.integrations.refresh(job["provider"], stop=self.cancel_event,
                                                         progress=self._progress)]
            if not job["results"][0].get("ok"):
                job["error"] = job["results"][0].get("message", "Refresh failed")
            return
        roots = [self.converter.roots[int(index)] for index in job["root_ids"]] if job["root_ids"] else self.converter.roots
        paths = [self.safe_path(value, roots) for value in job["paths"]] if job["paths"] else list(candidates(roots))
        with self.lock:
            job["total"] = len(paths)
        self.converter.stop = self.cancel_event
        self.converter.progress = self._progress
        self.converter.selected_languages = job["languages"]
        if job["kind"] != "inspect":
            job["summary"] = self.converter.scan(paths, retry=job["retry"])
            return
        summary = {"inspected": 0, "failed": 0}
        results = []
        for path in paths:
            if self.cancel_event.is_set():
                break
            with self.lock:
                job["current"] = {"path": str(path), "status": "inspecting"}
            try:
                self.safe_path(str(path), roots)
                tracks = self.converter.describe(path)
                tracks = [dict(track, language=self.converter.short_language(track["language"])) for track in tracks]
                result = {"path": str(path), "name": path.name, "type": "video" if path.suffix.lower() in VIDEOS else "sidecar",
                          "root_id": next(root["id"] for root in self.roots() if path.is_relative_to(Path(root["path"]))),
                          "tracks": tracks}
                summary["inspected"] += 1
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                result = {"path": str(path), "name": path.name, "tracks": [], "error": type(error).__name__}
                summary["failed"] += 1
            if len(results) < MAX_INVENTORY:
                results.append(result)
            with self.lock:
                job["processed"] += 1
                job["summary"] = dict(summary)
                job["results"] = list(results)
                job["truncated"] = len(paths) > MAX_INVENTORY

    def _worker(self):
        next_scan = time.monotonic()
        while not self.stop.is_set():
            if self.auto_scan and time.monotonic() >= next_scan:
                self.enqueue({"kind": "scan"}, origin="schedule")
                next_scan = time.monotonic() + self.interval
            with self.lock:
                job = self.queue.popleft() if self.queue else None
                if job:
                    self.running = job
                    self.cancel_event = threading.Event()
                    job.update(status="running", started=time.time())
            if not job:
                self.wake.wait(timeout=1)
                self.wake.clear()
                continue
            try:
                self._execute(job)
                job["status"] = "cancelled" if self.cancel_event.is_set() else "failed" if job.get("error") else "completed"
            except Exception as error:
                job.update(status="cancelled" if self.cancel_event.is_set() else "failed", error=type(error).__name__)
                LOG.warning("Dashboard job failed (%s)", type(error).__name__)
            finally:
                with self.lock:
                    job["finished"] = time.time()
                    self.history.append(job)
                    self.running = None
                    try:
                        # Inventory is live UI data; avoid persisting a large catalogue.
                        history = [dict(entry, results=[]) if entry["kind"] == "inspect" else entry for entry in self.history]
                        write_private(self.history_path, history)
                    except OSError:
                        LOG.warning("Dashboard history could not be saved")


class DashboardServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address, manager, password, allowed_hosts=None, webhook_token=""):
        self.manager = manager
        self.password = password
        self.allowed_hosts = set(allowed_hosts or ["localhost", "127.0.0.1", "::1", "subtitle-converter"])
        self.webhook_token = webhook_token
        self.slots = threading.BoundedSemaphore(32)
        super().__init__(address, Handler)

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        request.settimeout(10)
        try:
            super().process_request(request, client_address)
        except Exception:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()

    def handle_error(self, request, client_address):
        LOG.warning("Dashboard request failed")


class Handler(BaseHTTPRequestHandler):
    server_version = "SubtitleConverter"

    def log_message(self, *_):
        # URLs and request bodies can contain paths or credentials.
        pass

    def respond(self, status, data, *, content_type="application/json; charset=utf-8", extra=None):
        content = json.dumps(data).encode() if isinstance(data, (dict, list)) else data
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(content)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("X-Frame-Options", "DENY")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'")
        for name, value in (extra or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(content)

    def guard(self, *, hook=False):
        host = self.headers.get("Host", "")
        try:
            parsed = urlsplit("http://" + host)
            hostname = parsed.hostname
            _ = parsed.port
        except ValueError:
            hostname = None
        if not hostname or hostname.lower() not in self.server.allowed_hosts or parsed.username or parsed.path or parsed.query or parsed.fragment:
            self.respond(403, {"error": "Host is not allowed; configure CONVERTER_ALLOWED_HOSTS"})
            return False
        origin = self.headers.get("Origin")
        if origin and origin not in {"http://" + host, "https://" + host}:
            self.respond(403, {"error": "Cross-origin requests are not allowed"})
            return False
        if hook:
            token = self.server.webhook_token
            if not token or not hmac.compare_digest(self.headers.get("X-Webhook-Token", "").encode(), token.encode()):
                self.respond(401, {"error": "Webhook token is missing or invalid"})
                return False
            return True
        if self.path == "/health" and self.command == "GET":
            return True
        try:
            auth = self.headers.get("Authorization", "")
            if not auth.startswith("Basic "):
                raise ValueError("Missing authentication")
            supplied = base64.b64decode(auth[6:], validate=True).decode("utf-8")
            username, password = supplied.split(":", 1)
            authenticated = hmac.compare_digest(username.encode(), b"admin") and hmac.compare_digest(password.encode(), self.server.password.encode())
        except (ValueError, UnicodeError):
            authenticated = False
        if not authenticated:
            self.respond(401, {"error": "Dashboard login required"}, extra={"WWW-Authenticate": 'Basic realm="Subforge", charset="UTF-8"'})
            return False
        if self.command != "GET" and not hmac.compare_digest(self.headers.get("X-CSRF-Token", "").encode(), self.server.manager.csrf.encode()):
            self.respond(403, {"error": "Reload the dashboard before making changes"})
            return False
        return True

    def body(self):
        if self.headers.get("Transfer-Encoding"):
            raise ValueError("Transfer encoding is not supported")
        if self.headers.get("Content-Type", "").split(";")[0].strip() != "application/json":
            raise ValueError("Send an application/json body")
        size = int(self.headers.get("Content-Length", "0"))
        if not 0 < size <= MAX_BODY:
            raise ValueError("Request body must be between 1 and 65536 bytes")
        data = json.loads(self.rfile.read(size))
        if not isinstance(data, dict):
            raise ValueError("Expected a JSON object")
        return data

    def do_GET(self):
        if not self.guard():
            return
        routes = {"/": ("index.html", "text/html; charset=utf-8"),
                  "/static/app.js": ("app.js", "text/javascript; charset=utf-8"),
                  "/static/style.css": ("style.css", "text/css; charset=utf-8"),
                  "/static/icon.svg": ("icon.svg", "image/svg+xml"),
                  "/app.js": ("app.js", "text/javascript; charset=utf-8"),
                  "/style.css": ("style.css", "text/css; charset=utf-8")}
        try:
            if self.path in routes:
                name, content_type = routes[self.path]
                self.respond(200, (WEB / name).read_bytes(), content_type=content_type)
            elif self.path == "/health":
                self.respond(200, {"ok": True})
            elif self.path == "/favicon.ico":
                self.respond(204, b"", content_type="image/x-icon")
            elif self.path == "/api/session":
                self.respond(200, {"csrf_token": self.server.manager.csrf})
            elif self.path == "/api/status":
                self.respond(200, self.server.manager.status())
            elif self.path == "/api/settings":
                self.respond(200, self.server.manager.settings())
            elif self.path == "/api/integrations/jellyfin/libraries":
                self.respond(200, {"libraries": self.server.manager.integrations.libraries()})
            else:
                self.respond(404, {"error": "Not found"})
        except (OSError, ValueError, RuntimeError, HTTPError) as error:
            self.respond(400, {"error": str(error) if isinstance(error, ValueError) else "Request failed; check connection settings"})

    def do_PUT(self):
        if not self.guard():
            return
        try:
            if self.path != "/api/settings":
                self.respond(404, {"error": "Not found"})
                return
            self.respond(200, self.server.manager.update_settings(self.body()))
        except (OSError, ValueError, RuntimeError) as error:
            self.respond(400, {"error": str(error) if isinstance(error, ValueError) else "Settings could not be saved"})

    def do_POST(self):
        hook = self.path in {"/hooks/sonarr", "/hooks/radarr"}
        if not self.guard(hook=hook):
            return
        try:
            data = self.body()
            manager = self.server.manager
            if hook:
                provider = self.path.rsplit("/", 1)[1]
                paths = manager.integrations.webhook_paths(provider, data)
                if str(data.get("eventType", "")).lower() == "test":
                    self.respond(200, {"ok": True})
                    return
                mapped = [manager.integrations.map_path(provider, path) or path for path in paths]
                if not mapped or any(path is None for path in mapped):
                    raise ValueError("Webhook has no mapped media files")
                job = manager.enqueue({"kind": "extract", "paths": mapped}, origin=provider)
                self.respond(202, {"job": job})
            elif self.path == "/api/jobs":
                self.respond(202, {"job": manager.enqueue(data)})
            elif self.path.startswith("/api/jobs/") and self.path.endswith("/cancel"):
                identifier = self.path.split("/")[3]
                self.respond(200, manager.cancel(identifier))
            elif self.path.startswith("/api/integrations/"):
                parts = self.path.split("/")
                if len(parts) != 5 or parts[3] not in {"sonarr", "radarr", "jellyfin"}:
                    raise ValueError("Unknown integration endpoint")
                provider, action = parts[3:]
                if action == "test":
                    self.respond(200, manager.integrations.test(provider))
                elif action == "refresh":
                    self.respond(202, {"job": manager.enqueue({"kind": "refresh", "provider": provider})})
                else:
                    self.respond(404, {"error": "Not found"})
            else:
                self.respond(404, {"error": "Not found"})
        except (OSError, ValueError, RuntimeError) as error:
            self.respond(400, {"error": str(error) if isinstance(error, ValueError) else "Request failed; check connection settings"})


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", action="append")
    parser.add_argument("--data", default=os.environ.get("CONVERTER_DATA", "/data"))
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8100)
    parser.add_argument("--no-auto-scan", action="store_true", help="Only run manually requested jobs")
    args = parser.parse_args(argv)
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    data = Path(args.data)
    phase = "dashboard password and state storage"
    try:
        password = ui_password(data)
        phase = "scan settings"
        interval = interval_setting("CONVERTER_SCAN_INTERVAL", 300, 1)
        min_age = interval_setting("CONVERTER_MIN_AGE", 120, 0)
        timeout = interval_setting("CONVERTER_TIMEOUT", 7200, 1)
        retry = interval_setting("CONVERTER_RETRY_INTERVAL", 3600, 1)
        fallback = os.environ.get("CONVERTER_OCR_LANGUAGE", "eng")
        if not re.fullmatch(r"[a-zA-Z_]+(?:\+[a-zA-Z_]+)*", fallback):
            raise StartupError("CONVERTER_OCR_LANGUAGE must contain Tesseract language codes separated by '+' (for example eng+fra)")
        phase = "conversion tools"
        tools = {name: shutil.which(os.environ.get("CONVERTER_" + name.upper(), name)) for name in ("seconv", "ffprobe", "mkvmerge")}
        missing = [name for name, value in tools.items() if value is None]
        if missing:
            raise StartupError("Required conversion tools were not found: " + ", ".join(missing) + "; install them or configure their CONVERTER_* executable paths")
        phase = "converter state"
        converter = Converter(args.root or ["/movies", "/movies-C", "/tv", "/tv-C"], data / "state.json",
                              min_age=min_age, timeout=timeout, retry_interval=retry, ocr_language=fallback, **tools)
        phase = "dashboard preferences and integration settings"
        manager = JobManager(converter, Integrations(data / "settings.json"), interval=interval, auto_scan=not args.no_auto_scan)
        allowed = [value.strip().lower() for value in os.environ.get("CONVERTER_ALLOWED_HOSTS", "localhost,127.0.0.1,::1,subtitle-converter").split(",") if value.strip()]
        phase = "dashboard listener"
        server = DashboardServer((args.host, args.port), manager, password, allowed,
                                 os.environ.get("CONVERTER_WEBHOOK_TOKEN", ""))
    except StartupError as error:
        parser.error(str(error))
    except OSError as error:
        parser.error(startup_os_error(phase, error))
    except ValueError:
        parser.error(f"Cannot initialize {phase}: invalid configuration; check the documented settings")
    log_handler = ActivityLog(manager)
    LOG.addHandler(log_handler)
    def shutdown(*_):
        threading.Thread(target=server.shutdown, daemon=True).start()
    signal.signal(signal.SIGTERM, shutdown)
    signal.signal(signal.SIGINT, shutdown)
    manager.start()
    LOG.info("Dashboard listening on port %d; username admin", args.port)
    try:
        server.serve_forever(poll_interval=0.2)
    finally:
        manager.close()
        server.server_close()
        LOG.removeHandler(log_handler)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
