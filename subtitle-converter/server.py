"""Authenticated local dashboard and serialized subtitle conversion jobs."""

from __future__ import annotations

import argparse
import base64
from collections import deque
import errno
import hashlib
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
from urllib.parse import parse_qs, urlsplit

from converter import Converter, SIDECARS, VIDEOS, candidates
from integrations import Integrations
from http_client import HTTPError

LOG = logging.getLogger("subtitle-converter")
WEB = Path(__file__).parent / "web"
MAX_BODY = 65536
MAX_INVENTORY = 2000
MAX_ROOTS = 32
MAX_FOLDERS = 200
MAX_ERROR_GROUPS = 24


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


def widget_token_setting():
    token = os.environ.get("CONVERTER_WIDGET_TOKEN", "")
    if token and (not 32 <= len(token) <= 256 or not token.isascii() or not token.isprintable()
                  or any(character.isspace() for character in token)
                  or not re.fullmatch(r"[A-Za-z0-9._~+/-]+=*", token)):
        raise StartupError("CONVERTER_WIDGET_TOKEN must be a 32–256 character bearer token or blank to disable the widget")
    return token


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
        self.converted_sources = {path for path, item in converter.state.items()
                                  if isinstance(path, str) and isinstance(item, dict) and item.get("ok") is True}
        # The converter's startup roots are immutable mount boundaries. The
        # dashboard may select these directories or safe descendants only.
        self.mounts = [self._validate_mount(root) for root in converter.roots]
        self.root_paths = list(self.mounts)
        try:
            saved = json.loads(self.preferences.read_text(encoding="utf-8"))
        except FileNotFoundError:
            saved = None
        except (OSError, ValueError):
            saved = {}
            self.root_paths = []
            LOG.warning("Dashboard preferences could not be loaded; scan roots disabled")
        if saved is not None:
            if not isinstance(saved, dict):
                self.root_paths = []
                saved = {}
                LOG.warning("Dashboard preferences are invalid; scan roots disabled")
            try:
                self.languages = converter.normalize_languages(saved.get("languages", ["*"]))
            except (ValueError, TypeError, AttributeError):
                # Language corruption must never change the root selection.
                self.languages = ["*"]
                LOG.warning("Dashboard language preferences are invalid; using all languages")
            if "root_paths" in saved:
                paths = saved["root_paths"]
                try:
                    if not isinstance(paths, list) or len(paths) > MAX_ROOTS:
                        raise ValueError
                    # Keep removed mounts visible as unavailable roots, but
                    # reject the whole configuration if any entry is invalid.
                    self.root_paths = [self._validate_root(value, require_exists=False) for value in paths]
                except (OSError, ValueError, TypeError):
                    self.root_paths = []
                    LOG.warning("Dashboard root preferences are invalid; scan roots disabled")
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
        return [self._root_info(root) for root in self.root_paths]

    @staticmethod
    def _has_symlink_component(path):
        path = Path(path)
        current = Path(path.anchor)
        for part in path.parts[1:]:
            current /= part
            try:
                if current.is_symlink() or (hasattr(current, "is_junction") and current.is_junction()):
                    return True
            except OSError:
                return True
        return False

    def _validate_mount(self, value):
        path = Path(value).absolute()
        if self._has_symlink_component(path):
            raise ValueError("Configured scan roots must not contain symlinks")
        if path.exists() and not path.is_dir():
            raise ValueError("Configured scan roots must be directories")
        if path.exists() and not os.access(path, os.R_OK | os.X_OK):
            raise ValueError("Configured scan roots must be readable directories")
        resolved = path.resolve(strict=False)
        if resolved == Path(resolved.anchor) or str(resolved).lower() in {
                "/etc", "/proc", "/sys", "/dev", "/bin", "/sbin", "/usr", "/var", "/root",
                "c:\\", "c:\\windows", "c:\\program files", "c:\\program files (x86)"}:
            raise ValueError("System directories cannot be used as media scan roots")
        return resolved

    def _validate_root(self, value, *, require_exists=True):
        if not isinstance(value, str) or not value or len(value) > 4096 or "\x00" in value:
            raise ValueError("Choose a directory inside a configured media mount")
        path = Path(value)
        if not path.is_absolute() or ".." in path.parts or self._has_symlink_component(path):
            raise ValueError("Symlink and parent traversal paths are not supported")
        try:
            resolved = path.resolve(strict=require_exists)
            exists = resolved.is_dir()
            readable = os.access(resolved, os.R_OK | os.X_OK)
        except (OSError, RuntimeError):
            raise ValueError("Directory is unavailable or unreadable") from None
        if not any(resolved == mount or resolved.is_relative_to(mount) for mount in self.mounts):
            raise ValueError("Directory is outside configured media mounts")
        if require_exists and (not exists or not readable):
            raise ValueError("Choose an existing readable directory")
        return resolved

    def _root_info(self, root):
        root = Path(root)
        exact_mount = next((index for index, mount in enumerate(self.mounts) if root == mount), None)
        identifier = str(exact_mount) if exact_mount is not None else "r" + hashlib.sha256(str(root).encode()).hexdigest()[:12]
        available = root.exists() and root.is_dir() and not self._has_symlink_component(root)
        readable = available and os.access(root, os.R_OK | os.X_OK)
        try:
            free_bytes = shutil.disk_usage(root).free if readable else None
        except OSError:
            free_bytes = None
        return {"id": identifier, "path": str(root), "label": root.name or str(root),
                "status": "ready" if readable else "unavailable", "free_bytes": free_bytes}

    def folders(self, value):
        if value is None:
            path = self.mounts[0] if self.mounts else None
        else:
            path = self._validate_root(value)
        if path is None:
            return {"path": "", "parent": None, "folders": []}
        parent = path.parent if any(path.parent == mount or path.parent.is_relative_to(mount) for mount in self.mounts) else None
        folders = []
        truncated = False
        try:
            with os.scandir(path) as entries:
                for entry in entries:
                    try:
                        if not entry.is_dir(follow_symlinks=False) or entry.is_symlink():
                            continue
                        if len(folders) >= MAX_FOLDERS:
                            truncated = True
                            break
                        child = self._validate_root(entry.path)
                    except (OSError, ValueError):
                        continue
                    folders.append({"name": entry.name, "path": str(child)})
        except OSError:
            raise ValueError("Directory is unavailable or unreadable") from None
        folders.sort(key=lambda item: item["name"].casefold())
        return {"path": str(path), "parent": str(parent) if parent else None, "folders": folders,
                "truncated": truncated}

    def save_preferences(self, *, languages=None, roots=None):
        with self.lock:
            write_private(self.preferences, {"languages": list(self.languages if languages is None else languages),
                                              "root_paths": [str(path) for path in (self.root_paths if roots is None else roots)]})

    def safe_path(self, value, roots=None):
        if not isinstance(value, str) or len(value) > 4096 or "\x00" in value:
            raise ValueError("Invalid media path")
        path = Path(value)
        if not path.is_absolute() or ".." in path.parts:
            raise ValueError("Choose an absolute file path inside a configured library")
        for root in (self.root_paths if roots is None else roots):
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
            if self._has_symlink_component(path) or not path.resolve().is_relative_to(root.resolve()):
                raise ValueError("Media path is outside its library")
            if not path.is_file() or path.suffix.lower() not in VIDEOS | SIDECARS:
                raise ValueError("Media file is missing or unsupported")
            return path
        raise ValueError("Media path is outside configured libraries")

    def settings(self):
        public = self.integrations.public_settings()
        providers = {name: public[name] for name in ("sonarr", "radarr", "jellyfin", "bazarr")}
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
            self.save_preferences(languages=languages)
            self.languages = languages
        return self.settings()

    def update_roots(self, paths):
        if not isinstance(paths, list) or len(paths) > MAX_ROOTS:
            raise ValueError(f"Select at most {MAX_ROOTS} scan folders")
        normalized = []
        for value in paths:
            root = self._validate_root(value)
            if root not in normalized:
                normalized.append(root)
        with self.lock:
            # Write first: failed persistence must not change the live scan set.
            self.save_preferences(roots=normalized)
            self.root_paths = normalized
        return {"roots": self.roots(), "mounts": self.mount_options()}

    def mount_options(self):
        return [{"path": str(root), "label": root.name or str(root), "status": self._root_info(root)["status"]}
                for root in self.mounts]

    def status(self):
        settings = self.settings()
        with self.lock:
            # Round-trip snapshot: request threads never serialize live worker data.
            snapshot = json.loads(json.dumps({"running": self.running, "queue": list(self.queue),
                                             "history": list(reversed(self.history)), "logs": list(self.logs),
                                             "roots": self.roots(), "settings": settings}))
            # Correct older history written before partial failures had a status.
            for job in snapshot["history"]:
                if job.get("status") == "completed":
                    job["status"] = self.finished_status(job)
            return snapshot

    @staticmethod
    def finished_status(job):
        if job.get("error") or (job.get("kind") == "refresh" and
                                any(item.get("ok") is False for item in job.get("results", []) if isinstance(item, dict))):
            return "failed"
        summary = job.get("summary", {})
        if (isinstance(summary, dict) and JobManager._widget_counter(summary.get("failed")) > 0) or any(
                item.get("ok") is False for item in job.get("notifications", []) if isinstance(item, dict)):
            return "completed_with_errors"
        return "completed"

    def widget_status(self):
        """A path-free, read-only aggregate for small external status widgets."""
        with self.lock:
            running = self.running
            current = None
            if isinstance(running, dict):
                processed = self._widget_counter(running.get("processed"))
                total = self._widget_counter(running.get("total"))
                current = {
                    "kind": running.get("kind") if running.get("kind") in {"scan", "inspect", "extract", "refresh"} else "unknown",
                    "status": running.get("status") if running.get("status") in {"queued", "running", "cancelling"} else "running",
                    "processed": processed,
                    "total": total,
                    "progress_percent": min(100, int(processed * 100 / total)) if total else None,
                }
            failures = 0
            for job in self.history:
                summary = job.get("summary", {}) if isinstance(job, dict) else {}
                count = self._widget_counter(summary.get("failed")) if isinstance(summary, dict) else 0
                if job.get("status") == "failed" and count == 0:
                    count = 1
                failures += count
            return {"converted_count": len(self.converted_sources), "current_run": current,
                    "queued_count": len(self.queue), "configured_root_count": len(self.root_paths),
                    "selected_languages": list(self.languages), "recent_failures_count": failures}

    @staticmethod
    def _widget_counter(value):
        return value if type(value) is int and value >= 0 else 0

    def enqueue(self, data, *, origin="manual"):
        kind = data.get("kind", "scan")
        if kind not in {"scan", "inspect", "extract", "refresh"}:
            raise ValueError("Unknown job type")
        provider = data.get("provider")
        if kind == "refresh" and provider not in {"sonarr", "radarr", "jellyfin", "bazarr"}:
            raise ValueError("Unknown integration")
        root_ids = data.get("root_ids", [])
        with self.lock:
            available_roots = self.roots()
            root_config_snapshot = tuple(str(root) for root in self.root_paths)
            root_lookup = {root["id"]: Path(root["path"]) for root in available_roots}
            if not isinstance(root_ids, list) or any(not isinstance(index, str) or index not in root_lookup for index in root_ids):
                raise ValueError("Invalid library selection")
            # Empty root_ids retains the dashboard's “all configured folders” job
            # behavior. An empty configured folder list itself stays empty.
            selected_roots = [root_lookup[index] for index in root_ids] if root_ids else [Path(root["path"]) for root in available_roots]
            selected_root_ids = [self._root_info(root)["id"] for root in selected_roots]
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
            if tuple(str(root) for root in self.root_paths) != root_config_snapshot:
                raise ValueError("Scan folders changed while preparing this job; retry it")
            if origin == "schedule" and (self.running or self.queue):
                return None
            if len(self.queue) >= 32:
                raise ValueError("Job queue is full")
            job = {"id": secrets.token_hex(8), "kind": kind, "status": "queued", "created": time.time(),
                   "started": None, "finished": None, "current": None, "summary": {}, "error": None,
                   "results": [], "languages": languages, "root_ids": selected_root_ids,
                   "root_paths": [str(root) for root in selected_roots], "paths": paths,
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
            if event.get("status") == "converted" and isinstance(event.get("path"), str):
                self.converted_sources.add(event["path"])
            if event.get("status") == "failed":
                self._record_file_error(self.running, event.get("error", "Conversion failed"), event.get("path", ""))
        if event.get("new_outputs"):
            notifications = self.integrations.notify(event["new_outputs"])
            with self.lock:
                self.running.setdefault("notifications", []).extend(notifications)
                self.running["notifications"] = self.running["notifications"][-200:]

    @staticmethod
    def _record_file_error(job, message, path):
        # Converter diagnostics are fixed, safe messages. Keep repeated failures
        # useful without retaining a second unbounded catalogue of media paths.
        message = str(message)[:240]
        groups = job.setdefault("file_errors", [])
        group = next((item for item in groups if item["message"] == message), None)
        if group is None:
            if len(groups) >= MAX_ERROR_GROUPS:
                job["other_file_errors"] = job.get("other_file_errors", 0) + 1
                return
            group = {"message": message, "count": 0, "examples": []}
            groups.append(group)
        group["count"] += 1
        name = Path(path).name[:240] if isinstance(path, str) and path else ""
        if name and len(group["examples"]) < 3 and name not in group["examples"]:
            group["examples"].append(name)

    def _execute(self, job):
        if job["kind"] == "refresh":
            job["results"] = [self.integrations.refresh(job["provider"], stop=self.cancel_event,
                                                         progress=self._progress)]
            return
        roots = [self._validate_root(str(root), require_exists=False)
                 for root in job.get("root_paths", [])]
        paths = [self.safe_path(value, roots) for value in job["paths"]] if job["paths"] else list(candidates(roots))
        with self.lock:
            job["total"] = len(paths)
        self.converter.stop = self.cancel_event
        self.converter.progress = self._progress
        self.converter.selected_languages = job["languages"]
        if job["kind"] != "inspect":
            # Revalidate every discovered candidate lazily immediately before
            # the converter fingerprints or opens it.
            checked_paths = (self.safe_path(str(path), roots) for path in paths)
            job["summary"] = self.converter.scan(checked_paths, retry=job["retry"])
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
                          "root_id": next(identifier for identifier, root in zip(job["root_ids"], roots)
                                          if path.is_relative_to(root)),
                          "tracks": tracks}
                summary["inspected"] += 1
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                result = {"path": str(path), "name": path.name, "tracks": [], "error": type(error).__name__}
                summary["failed"] += 1
                with self.lock:
                    self._record_file_error(job, type(error).__name__, str(path))
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
            with self.lock:
                has_roots = bool(self.root_paths)
            if self.auto_scan and has_roots and time.monotonic() >= next_scan:
                try:
                    self.enqueue({"kind": "scan"}, origin="schedule")
                except ValueError:
                    LOG.warning("Scheduled scan skipped; check the configured scan folders")
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
            except Exception as error:
                job["error"] = type(error).__name__
                LOG.warning("Dashboard job failed (%s)", type(error).__name__)
            finally:
                if job["kind"] in {"scan", "extract"}:
                    try:
                        notification = self.integrations.flush_bazarr(stop=self.cancel_event, progress=self._progress)
                    except Exception as error:
                        notification = {"provider": "bazarr", "ok": False,
                                        "message": "Bazarr batch notification failed (" + type(error).__name__ + ")"}
                    if notification is not None:
                        with self.lock:
                            job.setdefault("notifications", []).append(notification)
                with self.lock:
                    job["status"] = "cancelled" if self.cancel_event.is_set() else self.finished_status(job)
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

    def __init__(self, address, manager, password, allowed_hosts=None, webhook_token="", widget_token=""):
        self.manager = manager
        self.password = password
        self.allowed_hosts = set(allowed_hosts or ["localhost", "127.0.0.1", "::1", "subtitle-converter"])
        self.webhook_token = webhook_token
        self.widget_token = widget_token
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

    def guard(self, *, hook=False, widget=False):
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
        if widget:
            token = self.server.widget_token
            auth = self.headers.get("Authorization", "")
            scheme, separator, supplied = auth.partition(" ")
            if not token:
                self.respond(404, {"error": "Not found"})
                return False
            if (not separator or scheme.lower() != "bearer" or not supplied
                    or not hmac.compare_digest(supplied.encode("utf-8"), token.encode("utf-8"))):
                self.respond(401, {"error": "Widget token is missing or invalid"},
                             extra={"WWW-Authenticate": 'Bearer realm="Subforge widget"'})
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
        widget = self.path == "/api/widget"
        if not self.guard(widget=widget):
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
            elif self.path == "/api/widget":
                self.respond(200, self.server.manager.widget_status())
            elif self.path == "/api/settings":
                self.respond(200, self.server.manager.settings())
            elif self.path == "/api/root-folders":
                manager = self.server.manager
                self.respond(200, {"roots": manager.roots(), "mounts": manager.mount_options()})
            elif self.path == "/api/folders" or self.path.startswith("/api/folders?"):
                query = parse_qs(urlsplit(self.path).query, keep_blank_values=True)
                values = query.get("path", [])
                if len(values) > 1:
                    raise ValueError("Choose one folder path")
                self.respond(200, self.server.manager.folders(values[0] if values and values[0] else None))
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
                if self.path != "/api/root-folders":
                    self.respond(404, {"error": "Not found"})
                    return
                self.respond(200, self.server.manager.update_roots(self.body().get("paths")))
            else:
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
                if len(parts) != 5 or parts[3] not in {"sonarr", "radarr", "jellyfin", "bazarr"}:
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
        widget_token = widget_token_setting()
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
                                 os.environ.get("CONVERTER_WEBHOOK_TOKEN", ""), widget_token)
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
