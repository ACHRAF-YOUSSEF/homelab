"""Optional, server-side Sonarr, Radarr, Bazarr and Jellyfin integrations."""

from __future__ import annotations

import json
import copy
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
import tempfile
import threading
from urllib.parse import quote, urlencode

from http_client import HTTPError, request, validate_base_url


DEFAULTS = {
    "sonarr": {"enabled": False, "url": os.environ.get("SONARR_INTERNAL_URL", ""),
               "api_key": os.environ.get("SONARR_API_KEY", "")},
    "radarr": {"enabled": False, "url": os.environ.get("RADARR_INTERNAL_URL", ""),
               "api_key": os.environ.get("RADARR_API_KEY", "")},
    "bazarr": {"enabled": False, "url": os.environ.get("BAZARR_INTERNAL_URL", "http://bazarr:6767"),
               "api_key": os.environ.get("BAZARR_API_KEY", "")},
    "jellyfin": {"enabled": False,
                 "url": os.environ.get("JELLYFIN_INTERNAL_URL", "http://host.docker.internal:8096"),
                 "api_key": os.environ.get("JELLYFIN_API_KEY", ""), "movie_libraries": [],
                 "series_libraries": [], "refresh_movies": True, "refresh_series": True,
                 "refresh_mode": "immediate"},
    "path_mappings": [],
}
SAFE_KEY = re.compile(r"^[A-Za-z0-9._~+/=-]{1,512}$")


def _http_failure(exc, *, prefix="Connection failed"):
    """Turn a categorized HTTP failure into useful text without exposing details."""
    category = getattr(exc, "category", "service_error")
    status = getattr(exc, "status_code", None)
    messages = {
        "authentication": f"API key rejected (HTTP {status})" if status else "API key rejected (HTTP 401/403)",
        "refused": "Connection refused; check the service URL and availability",
        "unreachable": "Could not reach the configured service; check its URL and network",
        "timeout": "Request timed out while waiting for the service",
        "invalid_json": "Service returned an invalid JSON response; check its URL and proxy settings",
        "response_limit": "Service response exceeded the safety limit",
        "http_status": f"Service returned HTTP {status}" if status else "Service returned an HTTP error",
    }
    return f"{prefix}: {messages.get(category, 'check the URL, API key, and service logs')}"


def _norm(path):
    """Return normalized absolute path and path flavor for prefix-safe matching."""
    value = str(path or "").strip()
    if not value or "\x00" in value:
        return None, None
    win = PureWindowsPath(value)
    if win.drive:
        return str(win).replace("\\", "/").rstrip("/"), "windows"
    posix = PurePosixPath(value)
    if not posix.is_absolute():
        return None, None
    return str(posix).rstrip("/") or "/", "posix"


def _under(path, root, flavor):
    if flavor == "windows":
        path, root = path.casefold(), root.casefold()
    return path == root or path.startswith(root.rstrip("/") + "/")


class Integrations:
    """Persist integrations and trigger scoped media-server refreshes.

    ``settings_path`` should be a private file in the service state directory.
    Network errors are returned as sanitized status messages and do not escape
    ``notify`` into the converter's successful extraction path.
    """

    def __init__(self, settings_path):
        self.settings_path = Path(settings_path)
        self.settings_path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.RLock()
        self._bazarr_pending_paths = set()
        self._settings = self._load()

    def _load(self):
        values = json.loads(json.dumps(DEFAULTS))
        values["sonarr"]["url"] = os.environ.get("SONARR_INTERNAL_URL", values["sonarr"]["url"])
        values["sonarr"]["api_key"] = os.environ.get("SONARR_API_KEY", values["sonarr"]["api_key"])
        values["radarr"]["url"] = os.environ.get("RADARR_INTERNAL_URL", values["radarr"]["url"])
        values["radarr"]["api_key"] = os.environ.get("RADARR_API_KEY", values["radarr"]["api_key"])
        values["bazarr"]["url"] = os.environ.get("BAZARR_INTERNAL_URL", values["bazarr"]["url"])
        values["bazarr"]["api_key"] = os.environ.get("BAZARR_API_KEY", values["bazarr"]["api_key"])
        try:
            raw = json.loads(self.settings_path.read_text(encoding="utf-8"))
            if isinstance(raw, dict):
                for provider in ("sonarr", "radarr", "bazarr", "jellyfin"):
                    if isinstance(raw.get(provider), dict):
                        values[provider].update({k: v for k, v in raw[provider].items()
                                                 if not (provider == "jellyfin" and k in {"url", "api_key"})})
                if isinstance(raw.get("path_mappings"), list):
                    values["path_mappings"] = raw["path_mappings"]
        except FileNotFoundError:
            pass
        except (OSError, ValueError):
            pass
        # Jellyfin connection details are process configuration, never state.
        values["jellyfin"]["url"] = os.environ.get(
            "JELLYFIN_INTERNAL_URL", "http://host.docker.internal:8096")
        values["jellyfin"]["api_key"] = os.environ.get("JELLYFIN_API_KEY", "")
        for provider in ("sonarr", "radarr", "bazarr", "jellyfin"):
            conf = values[provider]
            conf["enabled"] = bool(conf.get("enabled"))
            conf["url"] = str(conf.get("url") or "")
            conf["api_key"] = str(conf.get("api_key") or "")
        return values

    def _save(self, settings=None):
        settings = settings if settings is not None else self._settings
        serialized = copy.deepcopy(settings)
        # A Jellyfin token or endpoint must never be written to the JSON file.
        serialized["jellyfin"].pop("url", None)
        serialized["jellyfin"].pop("api_key", None)
        self.settings_path.parent.mkdir(parents=True, exist_ok=True)
        fd, temporary = tempfile.mkstemp(prefix=".integrations-", dir=self.settings_path.parent)
        try:
            if os.name != "nt":
                os.fchmod(fd, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as stream:
                json.dump(serialized, stream, ensure_ascii=False, separators=(",", ":"))
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, self.settings_path)
            if os.name != "nt":
                os.chmod(self.settings_path, 0o600)
        finally:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass

    def public_settings(self):
        with self._lock:
            result = copy.deepcopy(self._settings)
        for provider in ("sonarr", "radarr", "bazarr", "jellyfin"):
            result[provider]["api_key_set"] = bool(result[provider].pop("api_key", ""))
            result[provider]["api_key"] = ""
        result["jellyfin"]["env_managed"] = True
        return result

    def update(self, settings):
        if not isinstance(settings, dict):
            raise ValueError("Settings must be an object")
        with self._lock:
            candidate = copy.deepcopy(self._settings)
            for provider in ("sonarr", "radarr", "bazarr", "jellyfin"):
                incoming = settings.get(provider)
                if incoming is None:
                    continue
                if not isinstance(incoming, dict):
                    raise ValueError("Integration settings must be objects")
                target = candidate[provider]
                if provider == "jellyfin":
                    if "url" in incoming:
                        supplied_url = str(incoming["url"] or "").strip()
                        current_url = os.environ.get("JELLYFIN_INTERNAL_URL", "http://host.docker.internal:8096")
                        if supplied_url.rstrip("/").casefold() != current_url.rstrip("/").casefold():
                            raise ValueError("Jellyfin URL is managed by environment variables")
                    if incoming.get("api_key") or incoming.get("clear_api_key"):
                        raise ValueError("Jellyfin API key is managed by environment variables")
                if "enabled" in incoming:
                    target["enabled"] = bool(incoming["enabled"])
                if provider != "jellyfin" and "url" in incoming:
                    url = str(incoming["url"] or "").strip()
                    new_url = validate_base_url(url) if url else ""
                    if new_url.rstrip("/") != target["url"].rstrip("/"):
                        target["api_key"] = ""
                    target["url"] = new_url
                if provider != "jellyfin":
                    if incoming.get("clear_api_key") is True:
                        target["api_key"] = ""
                    elif "api_key" in incoming and incoming["api_key"]:
                        key = str(incoming["api_key"])
                        if not SAFE_KEY.fullmatch(key):
                            raise ValueError("API key contains unsupported characters")
                        target["api_key"] = key
                if provider == "jellyfin":
                    for key in ("movie_libraries", "series_libraries"):
                        if key in incoming:
                            values = incoming[key]
                            if not isinstance(values, list) or len(values) > 100:
                                raise ValueError("Library selections must be a list")
                            target[key] = [str(item)[:128] for item in values]
                    for key in ("refresh_movies", "refresh_series"):
                        if key in incoming:
                            target[key] = bool(incoming[key])
                    if "refresh_mode" in incoming:
                        if incoming["refresh_mode"] not in {"immediate", "async"}:
                            raise ValueError("Unsupported Jellyfin refresh mode")
                        target["refresh_mode"] = incoming["refresh_mode"]
            if "path_mappings" in settings:
                mappings = settings["path_mappings"]
                if not isinstance(mappings, list) or len(mappings) > 100:
                    raise ValueError("Path mappings must be a list")
                safe = []
                for mapping in mappings:
                    if not isinstance(mapping, dict):
                        raise ValueError("Invalid path mapping")
                    remote, remote_flavor = _norm(mapping.get("remote"))
                    local, local_flavor = _norm(mapping.get("local"))
                    if not remote or not local:
                        raise ValueError("Path mappings must use absolute paths")
                    mapping_provider = mapping.get("provider")
                    if mapping_provider not in {None, "sonarr", "radarr", "bazarr", "jellyfin"}:
                        raise ValueError("Path mapping provider must be Sonarr, Radarr, Bazarr, or Jellyfin")
                    safe.append({"remote": str(mapping["remote"]).rstrip("/\\"),
                                 "local": str(mapping["local"]).rstrip("/\\"),
                                 **({"provider": mapping_provider} if mapping_provider else {})})
                candidate["path_mappings"] = safe
            self._save(candidate)
            self._settings = candidate
        return self.public_settings()

    def map_path(self, provider, remote):
        if provider not in {"sonarr", "radarr", "bazarr", "jellyfin"}:
            return None
        path, flavor = _norm(remote)
        if path is None:
            return None
        mappings = sorted(self._settings["path_mappings"],
                          key=lambda m: len(m.get("remote", "")), reverse=True)
        for mapping in mappings:
            if mapping.get("provider") not in {None, provider}:
                continue
            root, root_flavor = _norm(mapping.get("remote"))
            local, local_flavor = _norm(mapping.get("local"))
            if root_flavor == flavor and _under(path, root, flavor):
                suffix = path[len(root):].lstrip("/")
                sep = "\\" if local_flavor == "windows" else "/"
                return str(mapping["local"]).rstrip("/\\") + (sep + suffix.replace("/", sep) if suffix else "")
        return None

    def remote_path(self, provider, local):
        """Translate a converter path back into the application's configured path."""
        if provider not in {"sonarr", "radarr", "bazarr", "jellyfin"}:
            return None
        path, flavor = _norm(local)
        if path is None:
            return None
        mappings = sorted(self._settings["path_mappings"],
                          key=lambda m: len(m.get("local", "")), reverse=True)
        for mapping in mappings:
            if mapping.get("provider") not in {None, provider}:
                continue
            root, root_flavor = _norm(mapping.get("local"))
            remote, remote_flavor = _norm(mapping.get("remote"))
            if root_flavor == flavor and _under(path, root, flavor):
                suffix = path[len(root):].lstrip("/")
                sep = "\\" if remote_flavor == "windows" else "/"
                return str(mapping["remote"]).rstrip("/\\") + (sep + suffix.replace("/", sep) if suffix else "")
        return local

    def webhook_paths(self, provider, payload):
        if provider not in {"sonarr", "radarr"} or not isinstance(payload, dict):
            return []
        event = str(payload.get("eventType", "")).lower()
        if event == "test":
            return []
        if event not in {"download", "upgrade", "rename"}:
            return []
        if event == "rename":
            key = "renamedEpisodeFiles" if provider == "sonarr" else "renamedMovieFiles"
            files = payload.get(key, [])
            if isinstance(files, list):
                return [item["path"] for item in files if isinstance(item, dict) and isinstance(item.get("path"), str)]
            return []
        if provider == "sonarr" and isinstance(payload.get("episodeFiles"), list):
            return [item["path"] for item in payload["episodeFiles"] if isinstance(item, dict) and isinstance(item.get("path"), str)]
        if provider == "sonarr":
            file = payload.get("episodeFile") or {}
            path = file.get("path") if isinstance(file, dict) else None
            if not path and event == "rename":
                series = payload.get("series") or {}
                path = series.get("path") if isinstance(series, dict) else None
        else:
            file = payload.get("movieFile") or {}
            path = file.get("path") if isinstance(file, dict) else None
            if not path and event == "rename":
                movie = payload.get("movie") or {}
                path = movie.get("path") if isinstance(movie, dict) else None
        return [str(path)] if path else []

    def _api(self, provider, path, method="GET", body=None):
        with self._lock:
            conf = copy.deepcopy(self._settings[provider])
        args = {"method": method, "api_key": conf["api_key"], "body": body}
        if provider == "jellyfin":
            args["api_key_header"] = "X-Emby-Token"
        return request(conf["url"], path, **args)

    def _bazarr_tasks(self):
        response = self._api("bazarr", "/api/system/tasks")
        tasks = response.get("data") if isinstance(response, dict) else None
        if not isinstance(tasks, list):
            raise HTTPError("Bazarr returned an invalid task list", category="invalid_json")
        return {item.get("job_id"): item for item in tasks
                if isinstance(item, dict) and isinstance(item.get("job_id"), str)}

    def _bazarr_run_task(self, task_id):
        # Bazarr's API parser consumes application/x-www-form-urlencoded fields.
        conf = self._settings["bazarr"]
        return request(conf["url"], "/api/system/tasks", method="POST", api_key=conf["api_key"],
                       api_key_header="X-Api-Key", form={"taskid": task_id})

    def _bazarr_action(self, media_kind, media_id, action):
        if media_kind == "series":
            path, key = "/api/series", "seriesid"
        elif media_kind == "movie":
            path, key = "/api/movies", "radarrid"
        else:
            raise ValueError("Unknown Bazarr media type")
        conf = self._settings["bazarr"]
        return request(conf["url"], path, method="PATCH", api_key=conf["api_key"],
                       api_key_header="X-Api-Key", form={key: str(media_id), "action": action})

    def _bazarr_media(self, media_kind, *, stop=None, progress=None, max_items=20000):
        """List Bazarr media with a strict cap; the API returns root/file paths."""
        endpoint = "/api/series" if media_kind == "series" else "/api/movies"
        records = []
        start = 0
        page_size = 200
        while start < max_items:
            if stop and stop.is_set():
                break
            query = urlencode({"start": start, "length": min(page_size, max_items - start)})
            result = self._api("bazarr", endpoint + "?" + query)
            page = result.get("data") if isinstance(result, dict) else None
            if not isinstance(page, list):
                raise HTTPError("Bazarr returned invalid media data", category="invalid_json")
            records.extend(item for item in page if isinstance(item, dict))
            total = result.get("total", 0)
            if not isinstance(total, int) or total < 0:
                raise HTTPError("Bazarr returned invalid media count", category="invalid_json")
            if len(records) >= total or len(page) < page_size:
                break
            start += len(page)
        if len(records) >= max_items and total > max_items:
            raise ValueError("Bazarr media list exceeds the configured scan limit")
        return records

    def _bazarr_ids_for_paths(self, paths, *, stop=None, progress=None):
        series_records = self._bazarr_media("series", stop=stop)
        movie_records = self._bazarr_media("movie", stop=stop)
        series_roots = []
        for item in series_records:
            media_id = item.get("sonarrSeriesId")
            root, flavor = _norm(item.get("path"))
            if isinstance(media_id, int) and root:
                series_roots.append((media_id, root, flavor))
        movie_files = []
        for item in movie_records:
            media_id = item.get("radarrId")
            movie_path, flavor = _norm(item.get("path"))
            if isinstance(media_id, int) and movie_path:
                movie_files.append((media_id, movie_path, flavor))

        def path_key(value, flavor):
            normalized = str(value).replace("\\", "/")
            return normalized.casefold() if flavor == "windows" else normalized

        matched_series, matched_movies = set(), set()
        for path in paths:
            remote = self.remote_path("bazarr", path)
            normalized, flavor = _norm(remote)
            stem = self._subtitle_stem(remote)
            if not normalized or not stem:
                continue
            pure = PureWindowsPath(normalized) if flavor == "windows" else PurePosixPath(normalized)
            parent = path_key(pure.parent, flavor)
            for media_id, movie_path, movie_flavor in movie_files:
                movie = PureWindowsPath(movie_path) if movie_flavor == "windows" else PurePosixPath(movie_path)
                if movie_flavor == flavor and path_key(movie.parent, flavor) == parent and \
                        path_key(movie.stem, flavor) == path_key(stem, flavor):
                    matched_movies.add(media_id)
            matching_roots = [(len(root), media_id) for media_id, root, root_flavor in series_roots
                              if root_flavor == flavor and _under(normalized, root, flavor)]
            if matching_roots:
                matched_series.add(max(matching_roots)[1])
        return sorted(matched_series), sorted(matched_movies)

    def flush_bazarr(self, *, stop=None, progress=None):
        """Scan only Bazarr entities touched by SRTs from the completed job."""
        with self._lock:
            paths = sorted(self._bazarr_pending_paths)
            # Consume this batch before doing network work, so cancel/error can
            # never leak paths into the next conversion job.
            self._bazarr_pending_paths.clear()
            conf = copy.deepcopy(self._settings["bazarr"])
        if not paths:
            return None
        if not conf["enabled"] or not conf["url"] or not conf["api_key"]:
            return {"provider": "bazarr", "ok": True, "triggered": 0, "message": "Bazarr integration is disabled"}
        if stop and stop.is_set():
            return {"provider": "bazarr", "ok": True, "triggered": 0, "message": "Bazarr scan cancelled"}
        try:
            series_ids, movie_ids = self._bazarr_ids_for_paths(paths, stop=stop, progress=progress)
            actions = [("series", identifier) for identifier in series_ids]
            actions.extend(("movie", identifier) for identifier in movie_ids)
            done = 0
            for media_kind, identifier in actions:
                if stop and stop.is_set():
                    return {"provider": "bazarr", "ok": True, "triggered": done,
                            "message": "Bazarr scan cancelled"}
                if progress:
                    progress({"status": "refreshing", "path": f"Bazarr · {media_kind} {identifier}"})
                self._bazarr_action(media_kind, identifier, "sync")
                if stop and stop.is_set():
                    return {"provider": "bazarr", "ok": True, "triggered": done,
                            "message": "Bazarr scan cancelled"}
                self._bazarr_action(media_kind, identifier, "scan-disk")
                done += 1
            return {"provider": "bazarr", "ok": True, "triggered": done,
                    "message": f"Scanned {done} Bazarr item(s)" if done else "No matching Bazarr media item"}
        except HTTPError as exc:
            return {"provider": "bazarr", "ok": False, "triggered": 0,
                    "message": _http_failure(exc, prefix="Bazarr scan failed")}
        except (ValueError, TypeError, KeyError):
            return {"provider": "bazarr", "ok": False, "triggered": 0,
                    "message": "Bazarr scan failed; check media paths and integration settings"}

    def test(self, provider):
        if provider not in {"sonarr", "radarr", "bazarr", "jellyfin"}:
            return {"connected": False, "message": "Unknown integration"}
        conf = self._settings[provider]
        if not conf["url"] or not conf["api_key"]:
            return {"connected": False, "message": "Configure the server URL and API key"}
        try:
            if provider == "sonarr":
                data = self._api(provider, "/api/v3/system/status")
            elif provider == "radarr":
                data = self._api(provider, "/api/v3/system/status")
            elif provider == "bazarr":
                data = self._api(provider, "/api/system/status")
                data = data.get("data") if isinstance(data, dict) else None
            else:
                data = self._api(provider, "/System/Info")
            if not isinstance(data, dict):
                raise HTTPError("Invalid service status", category="invalid_json")
            version = data.get("bazarr_version") if provider == "bazarr" else data.get("version", data.get("Version", ""))
            return {"connected": True, "version": str(version or "")[:64],
                    "message": "Connected"}
        except HTTPError as exc:
            return {"connected": False, "message": _http_failure(exc)}
        except ValueError:
            return {"connected": False, "message": "Connection failed; check the URL, API key, and service logs"}

    def _bazarr_refresh(self, *, stop=None, progress=None):
        tasks = self._bazarr_tasks()
        sync_tasks = [task for task in ("update_series", "update_movies") if task in tasks]
        scan_tasks = [task for task in ("series_full_scan_subtitles", "movies_full_scan_subtitles")
                      if task in tasks]
        if not scan_tasks:
            return {"provider": "bazarr", "ok": False,
                    "message": "Bazarr has no subtitle-index scan tasks; connect it to Sonarr or Radarr"}
        queued = 0
        for task_id in sync_tasks + scan_tasks:
            if stop and stop.is_set():
                return {"provider": "bazarr", "ok": True, "message": "Bazarr refresh cancelled before all tasks were queued"}
            task = tasks[task_id]
            if task.get("job_running"):
                continue
            if progress:
                progress({"status": "refreshing", "path": f"Bazarr · {task.get('name', task_id)}"})
            self._bazarr_run_task(task_id)
            queued += 1
        if not queued:
            return {"provider": "bazarr", "ok": True, "message": "Bazarr sync and subtitle-index tasks are already running"}
        return {"provider": "bazarr", "ok": True,
                "message": f"Queued {queued} Bazarr sync and subtitle-index task(s)"}

    def libraries(self):
        conf = self._settings["jellyfin"]
        if not conf["url"] or not conf["api_key"]:
            raise ValueError("Configure Jellyfin before loading libraries")
        folders = self._api("jellyfin", "/Library/VirtualFolders")
        results = []
        for folder in folders if isinstance(folders, list) else []:
            if not isinstance(folder, dict):
                continue
            name = str(folder.get("Name", ""))[:128]
            collection = str(folder.get("CollectionType", "") or "").lower()
            kind = "movies" if collection == "movies" else "series" if collection in {"tvshows", "tv"} else "other"
            results.append({"id": str(folder.get("ItemId", "")), "name": name, "type": kind,
                            "locations": [str(p) for p in folder.get("Locations", []) if isinstance(p, str)]})
        return results

    def _arr_refresh(self, provider, paths):
        endpoint = "/api/v3/series" if provider == "sonarr" else "/api/v3/movie"
        entities = self._api(provider, endpoint)
        ids = set()
        for entity in entities if isinstance(entities, list) else []:
            if not isinstance(entity, dict):
                continue
            root, flavor = _norm(entity.get("path"))
            if root is None:
                continue
            for path in paths:
                candidate, candidate_flavor = _norm(self.remote_path(provider, path))
                if candidate and candidate_flavor == flavor and _under(candidate, root, flavor):
                    ids.add((entity.get("id"), root))
                    break
        command = "RescanSeries" if provider == "sonarr" else "RescanMovie"
        for entity_id, _ in sorted(ids, key=lambda item: str(item[0])):
            if isinstance(entity_id, int):
                self._api(provider, "/api/v3/command", method="POST",
                          body={"name": command, "seriesId" if provider == "sonarr" else "movieId": entity_id})
        return len(ids)

    @staticmethod
    def _jellyfin_library_ids(conf, kind):
        if kind == "movies":
            return set(conf["movie_libraries"]) if conf["refresh_movies"] else set()
        return set(conf["series_libraries"]) if conf["refresh_series"] else set()

    @staticmethod
    def _subtitle_stem(path):
        normalized, flavor = _norm(path)
        if normalized is None:
            return ""
        pure = PureWindowsPath(normalized) if flavor == "windows" else PurePosixPath(normalized)
        name = pure.name
        if not name.lower().endswith(".srt"):
            return ""
        stem = name[:-4]
        # The converter appends language, duplicate-track, and accessibility tags.
        stem = re.sub(r"(?:\.(?:forced|hi|sdh|cc))+$", "", stem, flags=re.IGNORECASE)
        stem = re.sub(r"\.track\d+$", "", stem, flags=re.IGNORECASE)
        stem = re.sub(r"\.[a-z]{2,3}(?:-[a-z0-9]+)*$", "", stem, flags=re.IGNORECASE)
        stem = re.sub(r"\.track\d+$", "", stem, flags=re.IGNORECASE)
        return stem

    @staticmethod
    def _jellyfin_items_result(data):
        if isinstance(data, dict):
            items = data.get("Items", data.get("items", []))
            return [item for item in items if isinstance(item, dict)] if isinstance(items, list) else []
        return []

    def _resolve_jellyfin_item(self, library, path):
        """Resolve the containing media item by searching its selected library."""
        mapped = self.remote_path("jellyfin", path)
        expected = _norm(mapped)[0]
        stem = self._subtitle_stem(mapped)
        if not expected or not stem:
            return None
        flavor = _norm(mapped)[1]
        pure = PureWindowsPath(expected) if flavor == "windows" else PurePosixPath(expected)
        query = urlencode({"ParentId": library["id"], "Recursive": "true", "Fields": "Path",
                           "SearchTerm": stem, "Limit": "100", "EnableTotalRecordCount": "false"})
        data = self._api("jellyfin", "/Items?" + query)
        expected_parent = str(pure.parent).replace("\\", "/").casefold()
        for item in self._jellyfin_items_result(data):
            item_path = item.get("Path") or item.get("path")
            item_id = item.get("Id") or item.get("id")
            item_norm, item_flavor = _norm(item_path)
            if not item_norm or item_flavor != flavor or not item_id:
                continue
            item_pure = PureWindowsPath(item_norm) if flavor == "windows" else PurePosixPath(item_norm)
            parent = str(item_pure.parent).replace("\\", "/").casefold()
            if parent == expected_parent and item_pure.stem.casefold() == stem.casefold():
                return str(item_id)
        return None

    def _jellyfin_refresh(self, paths):
        with self._lock:
            conf = copy.deepcopy(self._settings["jellyfin"])
        if not paths or not conf["api_key"]:
            return 0
        libraries = self.libraries()
        chosen_paths = {}
        for folder in libraries:
            ids = self._jellyfin_library_ids(conf, folder["type"])
            if folder["type"] not in {"movies", "series"} or folder["id"] not in ids:
                continue
            for location in folder["locations"]:
                root = self.map_path("jellyfin", location) or location
                root_norm, flavor = _norm(root)
                matching = [path for path in paths if (_norm(path)[0] is not None and
                            _norm(path)[1] == flavor and _under(_norm(path)[0], root_norm, flavor))]
                if matching:
                    chosen_paths.setdefault(folder["id"], {"folder": folder, "paths": set()})["paths"].update(matching)
        if not chosen_paths:
            return 0
        scoped_paths = sorted(set().union(*(value["paths"] for value in chosen_paths.values())))
        if conf["refresh_mode"] == "async":
            updates = []
            for path in scoped_paths:
                remote = self.remote_path("jellyfin", path)
                if remote:
                    updates.append({"Path": remote, "UpdateType": "Created"})
            if updates:
                self._api("jellyfin", "/Library/Media/Updated", method="POST", body={"Updates": updates})
                return len(updates)
            return 0
        refreshed = set()
        for scoped in chosen_paths.values():
            folder = scoped["folder"]
            for path in scoped["paths"]:
                media_id = self._resolve_jellyfin_item(folder, path)
                if media_id:
                    query = urlencode({"MetadataRefreshMode": "ValidationOnly",
                                       "ImageRefreshMode": "ValidationOnly",
                                       "ReplaceAllMetadata": "false", "ReplaceAllImages": "false"})
                    self._api("jellyfin", f"/Items/{quote(media_id, safe='')}/Refresh?{query}", method="POST")
                    refreshed.add(media_id)
        if not refreshed:
            # Jellyfin can still notice a new sidecar without a full library scan.
            updates = [{"Path": self.remote_path("jellyfin", path), "UpdateType": "Created"}
                       for path in scoped_paths if self.remote_path("jellyfin", path)]
            if updates:
                self._api("jellyfin", "/Library/Media/Updated", method="POST", body={"Updates": updates})
                return len(updates)
        return len(refreshed)

    def notify(self, paths):
        """Trigger configured scans; return safe per-provider status objects."""
        results = []
        paths = [str(path) for path in paths if isinstance(path, (str, os.PathLike))]
        with self._lock:
            bazarr = self._settings["bazarr"]
            if paths and bazarr["enabled"] and bazarr["url"] and bazarr["api_key"]:
                self._bazarr_pending_paths.update(paths)
        for provider in ("sonarr", "radarr", "jellyfin"):
            conf = self._settings[provider]
            if not conf["enabled"] or not conf["url"] or not conf["api_key"]:
                continue
            enabled_paths = paths
            try:
                if provider in {"sonarr", "radarr"}:
                    count = self._arr_refresh(provider, enabled_paths)
                else:
                    count = self._jellyfin_refresh(enabled_paths)
                results.append({"provider": provider, "ok": True, "triggered": count,
                                "message": "Scan requested" if count else "No matching media item"})
            except HTTPError as exc:
                results.append({"provider": provider, "ok": False, "triggered": 0,
                                "message": _http_failure(exc, prefix="Scan request failed")})
            except (ValueError, TypeError, KeyError):
                results.append({"provider": provider, "ok": False, "triggered": 0,
                                "message": "Scan request failed; check integration settings"})
        return results

    def refresh(self, provider, *, stop=None, progress=None):
        """Manually request a complete library scan/refresh from one service."""
        if provider not in {"sonarr", "radarr", "bazarr", "jellyfin"}:
            return {"provider": str(provider), "ok": False, "message": "Unknown integration"}
        with self._lock:
            conf = copy.deepcopy(self._settings[provider])
        if not conf["url"] or not conf["api_key"]:
            return {"provider": provider, "ok": False, "message": "Configure the server URL and API key"}
        try:
            if provider == "bazarr":
                return self._bazarr_refresh(stop=stop, progress=progress)
            if provider == "jellyfin":
                selected = []
                for folder in self.libraries():
                    ids = self._jellyfin_library_ids(conf, folder["type"])
                    if folder["id"] in ids:
                        selected.append(folder["id"])
                for library_id in selected:
                    # Recursive belongs to GET /Items, not POST Item/Refresh.
                    # Refresh actual movie/episode children to discover sidecars.
                    start = 0
                    while start < 20000:
                        if stop and stop.is_set():
                            return {"provider": provider, "ok": True, "message": "Refresh cancelled"}
                        query = urlencode({"ParentId": library_id, "Recursive": "true",
                                           "IncludeItemTypes": "Movie,Episode", "StartIndex": start,
                                           "Limit": 200, "EnableTotalRecordCount": "false"})
                        items = self._jellyfin_items_result(self._api(provider, "/Items?" + query))
                        for item in items:
                            if stop and stop.is_set():
                                return {"provider": provider, "ok": True, "message": "Refresh cancelled"}
                            identifier = item.get("Id") or item.get("id")
                            if not isinstance(identifier, str) or not identifier:
                                continue
                            query = urlencode({"MetadataRefreshMode": "ValidationOnly",
                                               "ImageRefreshMode": "ValidationOnly", "ReplaceAllMetadata": "false",
                                               "ReplaceAllImages": "false"})
                            self._api(provider, f"/Items/{quote(identifier, safe='')}/Refresh?{query}", method="POST")
                            if progress:
                                progress({"status": "refreshing", "path": provider,
                                          "output": str(item.get("Name", identifier))[:256]})
                        if len(items) < 200:
                            break
                        start += 200
                    if start >= 20000:
                        return {"provider": provider, "ok": False,
                                "message": "Library refresh reached its 20000-item limit; narrow the selected libraries"}
                if not selected:
                    return {"provider": provider, "ok": False,
                            "message": "Select at least one Jellyfin library to scan"}
                return {"provider": provider, "ok": True, "message": f"Scan requested for {len(selected)} libraries"}
            else:
                self._api(provider, "/api/v3/command", method="POST",
                          body={"name": "RescanSeries" if provider == "sonarr" else "RescanMovie"})
            return {"provider": provider, "ok": True, "message": "Scan requested"}
        except HTTPError as exc:
            return {"provider": provider, "ok": False,
                    "message": _http_failure(exc, prefix="Scan request failed")}
        except (ValueError, TypeError):
            return {"provider": provider, "ok": False,
                    "message": "Scan request failed; check integration settings"}
