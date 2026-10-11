"""Optional, server-side Sonarr, Radarr and Jellyfin integrations."""

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
    "jellyfin": {"enabled": False,
                 "url": os.environ.get("JELLYFIN_INTERNAL_URL", "http://host.docker.internal:8096"),
                 "api_key": os.environ.get("JELLYFIN_API_KEY", ""), "movie_libraries": [],
                 "series_libraries": [], "refresh_movies": True, "refresh_series": True,
                 "refresh_mode": "immediate"},
    "path_mappings": [],
}
SAFE_KEY = re.compile(r"^[A-Za-z0-9._~+/=-]{1,512}$")


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
        self._settings = self._load()

    def _load(self):
        values = json.loads(json.dumps(DEFAULTS))
        values["sonarr"]["url"] = os.environ.get("SONARR_INTERNAL_URL", values["sonarr"]["url"])
        values["sonarr"]["api_key"] = os.environ.get("SONARR_API_KEY", values["sonarr"]["api_key"])
        values["radarr"]["url"] = os.environ.get("RADARR_INTERNAL_URL", values["radarr"]["url"])
        values["radarr"]["api_key"] = os.environ.get("RADARR_API_KEY", values["radarr"]["api_key"])
        try:
            raw = json.loads(self.settings_path.read_text(encoding="utf-8"))
            if isinstance(raw, dict):
                for provider in ("sonarr", "radarr", "jellyfin"):
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
        for provider in ("sonarr", "radarr", "jellyfin"):
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
        for provider in ("sonarr", "radarr", "jellyfin"):
            result[provider]["api_key_set"] = bool(result[provider].pop("api_key", ""))
            result[provider]["api_key"] = ""
        result["jellyfin"]["env_managed"] = True
        return result

    def update(self, settings):
        if not isinstance(settings, dict):
            raise ValueError("Settings must be an object")
        with self._lock:
            candidate = copy.deepcopy(self._settings)
            for provider in ("sonarr", "radarr", "jellyfin"):
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
                    if mapping_provider not in {None, "sonarr", "radarr", "jellyfin"}:
                        raise ValueError("Path mapping provider must be Sonarr, Radarr, or Jellyfin")
                    safe.append({"remote": str(mapping["remote"]).rstrip("/\\"),
                                 "local": str(mapping["local"]).rstrip("/\\"),
                                 **({"provider": mapping_provider} if mapping_provider else {})})
                candidate["path_mappings"] = safe
            self._save(candidate)
            self._settings = candidate
        return self.public_settings()

    def map_path(self, provider, remote):
        if provider not in {"sonarr", "radarr", "jellyfin"}:
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
        if provider not in {"sonarr", "radarr", "jellyfin"}:
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

    def test(self, provider):
        if provider not in {"sonarr", "radarr", "jellyfin"}:
            return {"connected": False, "message": "Unknown integration"}
        conf = self._settings[provider]
        if not conf["url"] or not conf["api_key"]:
            return {"connected": False, "message": "Configure the server URL and API key"}
        try:
            if provider == "sonarr":
                data = self._api(provider, "/api/v3/system/status")
            elif provider == "radarr":
                data = self._api(provider, "/api/v3/system/status")
            else:
                data = self._api(provider, "/System/Info")
            if not isinstance(data, dict):
                raise HTTPError("Invalid service status")
            return {"connected": True, "version": str(data.get("version", data.get("Version", "")))[:64],
                    "message": "Connected"}
        except (HTTPError, ValueError):
            return {"connected": False, "message": "Connection failed; check the URL, API key, and service logs"}

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
        for provider in ("sonarr", "radarr", "jellyfin"):
            conf = self._settings[provider]
            if not conf["enabled"] or not conf["url"] or not conf["api_key"]:
                continue
            enabled_paths = paths
            try:
                count = self._arr_refresh(provider, enabled_paths) if provider in {"sonarr", "radarr"} else self._jellyfin_refresh(enabled_paths)
                results.append({"provider": provider, "ok": True, "triggered": count,
                                "message": "Scan requested" if count else "No matching media item"})
            except (HTTPError, ValueError, TypeError, KeyError):
                results.append({"provider": provider, "ok": False, "triggered": 0,
                                "message": "Scan request failed; check integration settings"})
        return results

    def refresh(self, provider, *, stop=None, progress=None):
        """Manually request a complete library scan/refresh from one service."""
        if provider not in {"sonarr", "radarr", "jellyfin"}:
            return {"provider": str(provider), "ok": False, "message": "Unknown integration"}
        with self._lock:
            conf = copy.deepcopy(self._settings[provider])
        if not conf["url"] or not conf["api_key"]:
            return {"provider": provider, "ok": False, "message": "Configure the server URL and API key"}
        try:
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
        except (HTTPError, ValueError, TypeError):
            return {"provider": provider, "ok": False,
                    "message": "Scan request failed; check integration settings"}
