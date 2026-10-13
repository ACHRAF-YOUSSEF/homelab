"""Tests for integration settings, webhook parsing, and scan requests."""

import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from http_client import HTTPError, request
from integrations import Integrations


class IntegrationTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self._env = patch.dict(os.environ, {"JELLYFIN_INTERNAL_URL": "http://jellyfin:8096",
                                            "JELLYFIN_API_KEY": "jf-secret",
                                            "BAZARR_INTERNAL_URL": "http://bazarr:6767",
                                            "BAZARR_API_KEY": ""})
        self._env.start()
        self.addCleanup(self._env.stop)
        self.path = Path(self.tmp.name) / "integrations.json"
        self.integration = Integrations(self.path)

    def test_keys_stay_private_and_blank_update_preserves(self):
        self.integration.update({"sonarr": {"url": "http://sonarr:8989", "api_key": "secret-key"}})
        self.integration.update({"sonarr": {"enabled": True, "api_key": ""}})
        public = self.integration.public_settings()
        self.assertTrue(public["sonarr"]["api_key_set"])
        self.assertEqual(public["sonarr"]["api_key"], "")
        self.assertNotIn("secret-key", json.dumps(public))
        self.assertIn("secret-key", self.path.read_text(encoding="utf-8"))
        if os.name != "nt":
            self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)

    def test_changing_service_url_clears_saved_key(self):
        self.integration.update({"sonarr": {"url": "http://sonarr:8989", "api_key": "secret-key"}})
        public = self.integration.update({"sonarr": {"url": "http://other:8989"}})
        self.assertFalse(public["sonarr"]["api_key_set"])

    def test_service_url_path_case_change_clears_saved_key(self):
        self.integration.update({"sonarr": {"url": "http://sonarr:8989/api", "api_key": "secret-key"}})
        public = self.integration.update({"sonarr": {"url": "http://sonarr:8989/Api"}})
        self.assertFalse(public["sonarr"]["api_key_set"])

    def test_failed_update_is_transactional(self):
        before = self.integration.public_settings()
        with self.assertRaises(ValueError):
            self.integration.update({"sonarr": {"enabled": True},
                                     "path_mappings": [{"remote": "relative", "local": "/local"}]})
        self.assertEqual(self.integration.public_settings(), before)

    def test_jellyfin_connection_is_environment_managed_and_not_persisted(self):
        self.integration.update({"jellyfin": {"enabled": True, "url": "http://jellyfin:8096/",
                                                 "api_key": "", "movie_libraries": ["m"]}})
        public = self.integration.public_settings()["jellyfin"]
        self.assertEqual(public["url"], "http://jellyfin:8096")
        self.assertTrue(public["env_managed"])
        self.assertTrue(public["api_key_set"])
        persisted = self.path.read_text(encoding="utf-8")
        self.assertNotIn("jf-secret", persisted)
        saved_jellyfin = json.loads(persisted)["jellyfin"]
        self.assertNotIn("url", saved_jellyfin)
        self.assertNotIn("api_key", saved_jellyfin)
        with self.assertRaises(ValueError):
            self.integration.update({"jellyfin": {"url": "http://elsewhere"}})
        with self.assertRaises(ValueError):
            self.integration.update({"jellyfin": {"api_key": "replacement"}})

    def test_bazarr_uses_prefilled_url_and_keeps_key_private(self):
        public = self.integration.update({"bazarr": {"enabled": True, "api_key": "bazarr-secret"}})
        self.assertEqual(public["bazarr"]["url"], "http://bazarr:6767")
        self.assertTrue(public["bazarr"]["enabled"])
        self.assertTrue(public["bazarr"]["api_key_set"])
        self.assertEqual(public["bazarr"]["api_key"], "")
        self.assertNotIn("bazarr-secret", json.dumps(public))
        self.assertIn("bazarr-secret", self.path.read_text(encoding="utf-8"))

    @patch("integrations.request")
    def test_bazarr_connection_uses_authenticated_status_endpoint(self, request_mock):
        self.integration.update({"bazarr": {"url": "http://bazarr:6767", "api_key": "bazarr-secret"}})
        request_mock.return_value = {"data": {"bazarr_version": "1.5.0"}}
        result = self.integration.test("bazarr")
        self.assertEqual(result, {"connected": True, "version": "1.5.0", "message": "Connected"})
        request_mock.assert_called_once_with("http://bazarr:6767", "/api/system/status",
                                             method="GET", api_key="bazarr-secret", body=None)

    @patch("integrations.request")
    def test_bazarr_manual_refresh_syncs_then_indexes_supported_tasks(self, request_mock):
        self.integration.update({"bazarr": {"url": "http://bazarr:6767", "api_key": "bazarr-secret"}})
        tasks = {"data": [{"job_id": task_id, "job_running": False, "name": task_id}
                          for task_id in ("update_series", "update_movies", "series_full_scan_subtitles",
                                          "movies_full_scan_subtitles")]}
        request_mock.side_effect = lambda _url, _path, **_kwargs: tasks
        result = self.integration.refresh("bazarr")
        self.assertTrue(result["ok"])
        posts = [call for call in request_mock.call_args_list if call.kwargs.get("method") == "POST"]
        self.assertEqual([call.kwargs["form"]["taskid"] for call in posts], [
            "update_series", "update_movies", "series_full_scan_subtitles", "movies_full_scan_subtitles"])
        self.assertTrue(all(call.kwargs["api_key_header"] == "X-Api-Key" for call in posts))

    @patch("integrations.request")
    def test_bazarr_auto_flush_syncs_and_scans_only_affected_series_and_movie(self, request_mock):
        self.integration.update({"bazarr": {"enabled": True, "url": "http://bazarr:6767",
                                               "api_key": "bazarr-secret"},
                                 "path_mappings": [{"provider": "bazarr", "remote": "/bazarr/tv",
                                                    "local": "/tv"}]})
        def respond(_url, path, **_kwargs):
            if path.startswith("/api/series?"):
                return {"data": [{"sonarrSeriesId": 12, "path": "/bazarr/tv/Show"}], "total": 1}
            if path.startswith("/api/movies?"):
                return {"data": [{"radarrId": 34, "path": "/movies/Film.mkv"}], "total": 1}
            return {}
        request_mock.side_effect = respond
        self.integration.notify(["/tv/Show/S01/ep.en.srt", "/movies/Film.en.srt"])
        result = self.integration.flush_bazarr()
        self.assertEqual(result["triggered"], 2)
        calls = [call for call in request_mock.call_args_list if call.kwargs.get("method") == "PATCH"]
        self.assertEqual([(call.args[1], call.kwargs["form"]) for call in calls], [
            ("/api/series", {"seriesid": "12", "action": "sync"}),
            ("/api/series", {"seriesid": "12", "action": "scan-disk"}),
            ("/api/movies", {"radarrid": "34", "action": "sync"}),
            ("/api/movies", {"radarrid": "34", "action": "scan-disk"}),
        ])
        self.assertTrue(all(call.kwargs["api_key_header"] == "X-Api-Key" for call in calls))

    @patch("integrations.request")
    def test_bazarr_posix_path_matching_preserves_case(self, request_mock):
        self.integration.update({"bazarr": {"enabled": True, "url": "http://bazarr:6767",
                                               "api_key": "bazarr-secret"}})
        def respond(_url, path, **_kwargs):
            if path.startswith("/api/series?"):
                return {"data": [], "total": 0}
            if path.startswith("/api/movies?"):
                return {"data": [
                    {"radarrId": 34, "path": "/movies/Film.mkv"},
                    {"radarrId": 35, "path": "/movies/film.mkv"},
                ], "total": 2}
            return {}
        request_mock.side_effect = respond
        self.integration.notify(["/movies/Film.en.srt"])
        result = self.integration.flush_bazarr()
        self.assertEqual(result["triggered"], 1)
        patches = [call.kwargs["form"] for call in request_mock.call_args_list
                   if call.kwargs.get("method") == "PATCH"]
        self.assertEqual(patches, [{"radarrid": "34", "action": "sync"},
                                   {"radarrid": "34", "action": "scan-disk"}])

    @patch("integrations.request")
    def test_bazarr_pending_paths_are_consumed_even_when_cancelled(self, request_mock):
        import threading
        self.integration.update({"bazarr": {"enabled": True, "url": "http://bazarr:6767",
                                               "api_key": "bazarr-secret"}})
        stop = threading.Event()
        stop.set()
        self.integration.notify(["/tv/Show/ep.en.srt"])
        result = self.integration.flush_bazarr(stop=stop)
        self.assertIn("cancelled", result["message"].lower())
        self.assertIsNone(self.integration.flush_bazarr())
        request_mock.assert_not_called()

    def test_map_path_uses_longest_boundary_match(self):
        self.integration.update({"path_mappings": [
            {"remote": "/data", "local": "/mnt/media"},
            {"remote": "/data/tv", "local": "/mnt/tv"},
        ]})
        self.assertEqual(self.integration.map_path("sonarr", "/data/tv/Show/ep.mkv"),
                         "/mnt/tv/Show/ep.mkv")
        self.assertEqual(self.integration.remote_path("sonarr", "/mnt/tv/Show/ep.mkv"),
                         "/data/tv/Show/ep.mkv")
        self.assertEqual(self.integration.map_path("sonarr", "/data2/tv/ep.mkv"), None)
        self.assertIsNone(self.integration.map_path("sonarr", "relative/path"))

    def test_windows_mapping_is_case_insensitive_and_boundary_safe(self):
        self.integration.update({"path_mappings": [
            {"remote": "Z:\\Media", "local": "D:\\Library"},
        ]})
        self.assertEqual(self.integration.map_path("radarr", "z:\\media\\Movie\\film.mkv"),
                         "D:\\Library\\Movie\\film.mkv")
        self.assertIsNone(self.integration.map_path("radarr", "Z:\\Media2\\film.mkv"))

    def test_cross_flavor_path_mapping_for_native_jellyfin_paths(self):
        self.integration.update({"path_mappings": [
            {"provider": "jellyfin", "remote": "Z:\\Media", "local": "/mnt/media"},
        ]})
        self.assertEqual(self.integration.map_path("jellyfin", "z:\\media\\Show\\ep.mkv"),
                         "/mnt/media/Show/ep.mkv")
        self.assertEqual(self.integration.remote_path("jellyfin", "/mnt/media/Show/ep.en.srt"),
                         "Z:\\Media\\Show\\ep.en.srt")

    def test_webhook_paths_accept_supported_events_only(self):
        self.assertEqual(self.integration.webhook_paths("sonarr", {"eventType": "Download",
                             "episodeFile": {"path": "/tv/show/ep.mkv"}}), ["/tv/show/ep.mkv"])
        self.assertEqual(self.integration.webhook_paths("radarr", {"eventType": "Test"}), [])
        self.assertEqual(self.integration.webhook_paths("radarr", {"eventType": "Grab",
                             "movieFile": {"path": "/movies/a.mkv"}}), [])

    def test_webhook_rename_and_batch_paths_filter_malformed_entries(self):
        self.assertEqual(self.integration.webhook_paths("sonarr", {
            "eventType": "Rename", "renamedEpisodeFiles": [
                {"path": "/tv/new.mkv"}, None, "bad", {"path": 42}, {},
            ],
        }), ["/tv/new.mkv"])
        self.assertEqual(self.integration.webhook_paths("radarr", {
            "eventType": "Rename", "renamedMovieFiles": [
                {"path": "/movies/new.mkv"}, {"path": None}, None,
            ],
        }), ["/movies/new.mkv"])
        self.assertEqual(self.integration.webhook_paths("sonarr", {
            "eventType": "Download", "episodeFiles": [
                {"path": "/tv/one.mkv"}, False, {"path": "/tv/two.mkv"}, {"path": []},
            ],
        }), ["/tv/one.mkv", "/tv/two.mkv"])

    @patch("integrations.request", return_value=[{"version": "unexpected-list"}])
    def test_test_status_requires_object_response(self, _request):
        self.integration.update({"sonarr": {"enabled": True, "url": "http://sonarr:8989",
                                               "api_key": "test-key"}})
        result = self.integration.test("sonarr")
        self.assertFalse(result["connected"])
        self.assertIn("Connection failed", result["message"])

    @patch("integrations.request")
    def test_arr_notifications_trigger_scoped_rescans_for_sidecars(self, request_mock):
        self.integration.update({
            "sonarr": {"enabled": True, "url": "http://sonarr:8989", "api_key": "s"},
            "radarr": {"enabled": True, "url": "http://radarr:7878", "api_key": "r"},
            "path_mappings": [
                {"provider": "sonarr", "remote": "/sonarr/tv", "local": "/tv"},
                {"provider": "radarr", "remote": "/radarr/movies", "local": "/movies"},
            ],
        })
        def respond(url, path, **kwargs):
            if path == "/api/v3/series":
                return [{"id": 12, "path": "/sonarr/tv/Show"}]
            if path == "/api/v3/movie":
                return [{"id": 34, "path": "/radarr/movies/Film"}]
            return {"status": "queued"}
        request_mock.side_effect = respond
        results = self.integration.notify(["/tv/Show/ep.en.srt", "/movies/Film/movie.fr.srt"])
        self.assertEqual([item["triggered"] for item in results], [1, 1])
        posts = [call for call in request_mock.call_args_list if call.kwargs.get("method") == "POST"]
        self.assertEqual(posts[0].kwargs["body"], {"name": "RescanSeries", "seriesId": 12})
        self.assertEqual(posts[1].kwargs["body"], {"name": "RescanMovie", "movieId": 34})

    @patch("integrations.request")
    def test_jellyfin_libraries_and_selected_refresh(self, request_mock):
        self.integration.update({"jellyfin": {"enabled": True,
            "movie_libraries": ["movies-id"], "series_libraries": [],
            "refresh_movies": True, "refresh_series": False}})
        def respond(url, path, **kwargs):
            if path == "/Library/VirtualFolders":
                return [
                    None,
                    "malformed-folder",
                    {"ItemId": "movies-id", "Name": "Films", "CollectionType": "movies",
                     "Locations": ["/jf/movies"]},
                    {"ItemId": "series-id", "Name": "Shows", "CollectionType": "tvshows",
                     "Locations": ["/jf/series"]},
                ]
            if path.startswith("/Items?"):
                if "SearchTerm=film" in path:
                    return {"Items": [{"Id": "movie-item", "Path": "/jf/movies/F/film.mkv"}]}
                return {"Items": []}
            return {}
        request_mock.side_effect = respond
        self.assertEqual(self.integration.libraries()[0], {
            "id": "movies-id", "name": "Films", "type": "movies", "locations": ["/jf/movies"]})
        result = self.integration.notify(["/jf/movies/F/film.en.srt"])[0]
        self.assertEqual(result["triggered"], 1)
        self.assertEqual(self.integration.notify(["/jf/series/S/ep.en.srt"])[0]["triggered"], 0)
        item_refreshes = [c for c in request_mock.call_args_list if "/Refresh?" in c.args[1]]
        self.assertEqual(len(item_refreshes), 1)
        self.assertIn("/Items/movie-item/Refresh?", item_refreshes[0].args[1])
        self.assertEqual(item_refreshes[0].kwargs["api_key_header"], "X-Emby-Token")

    @patch("integrations.request", side_effect=HTTPError("sensitive-url-secret"))
    def test_notify_failure_is_safe_and_does_not_raise(self, _request):
        self.integration.update({"sonarr": {"enabled": True, "url": "http://sonarr", "api_key": "secret"}})
        result = self.integration.notify(["/tv/show/sub.en.srt"])
        self.assertFalse(result[0]["ok"])
        self.assertNotIn("secret", json.dumps(result))

    @patch("integrations.request")
    def test_manual_refresh_triggers_full_jellyfin_scan(self, request_mock):
        self.integration.update({"jellyfin": {"movie_libraries": ["movies-id"]}})
        def response(_url, path, **_kwargs):
            if path == "/Library/VirtualFolders":
                return [{"ItemId": "movies-id", "Name": "Films", "CollectionType": "movies",
                         "Locations": ["/jf/movies"]}]
            if path.startswith("/Items?"):
                return {"Items": [{"Id": "child-item"}]}
            return {}
        request_mock.side_effect = response
        self.assertTrue(self.integration.refresh("jellyfin")["ok"])
        post = request_mock.call_args_list[-1]
        self.assertIn("/Items/child-item/Refresh?MetadataRefreshMode=ValidationOnly", post.args[1])
        query = request_mock.call_args_list[-2].args[1]
        self.assertIn("ParentId=movies-id", query)
        self.assertIn("Recursive=true", query)
        self.assertEqual(post.kwargs["api_key"], "jf-secret")

    @patch("integrations.request")
    def test_async_jellyfin_notification_is_path_scoped(self, request_mock):
        self.integration.update({"jellyfin": {"enabled": True, "movie_libraries": ["movies-id"],
            "refresh_movies": True, "refresh_series": True, "refresh_mode": "async"}})
        def respond(url, path, **kwargs):
            if path == "/Library/VirtualFolders":
                return [{"ItemId": "movies-id", "Name": "Films", "CollectionType": "movies",
                         "Locations": ["/jf/movies"]},
                        {"ItemId": "series-id", "Name": "Shows", "CollectionType": "tvshows",
                         "Locations": ["/jf/series"]}]
            return {}
        request_mock.side_effect = respond
        result = self.integration.notify(["/jf/movies/F/film.en.srt", "/jf/series/S/episode.en.srt"])[0]
        self.assertTrue(result["ok"])
        calls = [c for c in request_mock.call_args_list if c.args[1] == "/Library/Media/Updated"]
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0].kwargs["body"], {"Updates": [
            {"Path": "/jf/movies/F/film.en.srt", "UpdateType": "Created"}]})

    @patch("integrations.request")
    def test_manual_jellyfin_refresh_can_cancel_between_scoped_items(self, request_mock):
        import threading
        stop = threading.Event()
        self.integration.update({"jellyfin": {"movie_libraries": ["movies-id"], "series_libraries": []}})
        def response(_url, path, **kwargs):
            if path == "/Library/VirtualFolders":
                return [{"ItemId": "movies-id", "CollectionType": "movies", "Locations": ["/movies"]},
                        {"ItemId": "shows-id", "CollectionType": "tvshows", "Locations": ["/tv"]}]
            if path.startswith("/Items?"):
                self.assertIn("ParentId=movies-id", path)
                return {"Items": [{"Id": "first"}, {"Id": "second"}]}
            if kwargs.get("method") == "POST":
                stop.set()
            return {}
        request_mock.side_effect = response
        result = self.integration.refresh("jellyfin", stop=stop)
        self.assertIn("cancelled", result["message"].lower())
        posts = [call for call in request_mock.call_args_list if call.kwargs.get("method") == "POST"]
        self.assertEqual(len(posts), 1)
        self.assertIn("/Items/first/Refresh?", posts[0].args[1])

    def test_url_rejects_credentials_and_non_http_schemes(self):
        with self.assertRaises(ValueError):
            self.integration.update({"sonarr": {"url": "http://user:pass@example.test"}})
        with self.assertRaises(ValueError):
            self.integration.update({"sonarr": {"url": "file:///etc/passwd"}})


class HTTPClientTest(unittest.TestCase):
    def test_rejects_redirects_without_following_them(self):
        from http.server import BaseHTTPRequestHandler, HTTPServer
        import threading

        hits = []
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                hits.append(self.path)
                self.send_response(302)
                self.send_header("Location", "http://127.0.0.1:1/leak")
                self.end_headers()
            def log_message(self, *_args):
                pass
        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        def close_server():
            server.shutdown()
            thread.join(timeout=2)
            server.server_close()
        self.addCleanup(close_server)
        with self.assertRaises(HTTPError):
            request("http://127.0.0.1:%d" % server.server_port, "/test", timeout=2)
        self.assertEqual(hits, ["/test"])

    def test_posts_form_encoded_data(self):
        from http.server import BaseHTTPRequestHandler, HTTPServer
        import threading

        seen = []
        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                size = int(self.headers.get("Content-Length", "0"))
                seen.append((self.path, self.headers.get("Content-Type"), self.headers.get("X-Api-Key"),
                             self.rfile.read(size)))
                self.send_response(204)
                self.end_headers()
            def log_message(self, *_args):
                pass
        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        def close_server():
            server.shutdown()
            thread.join(timeout=2)
            server.server_close()
        self.addCleanup(close_server)
        result = request("http://127.0.0.1:%d" % server.server_port, "/api/system/tasks",
                         method="POST", api_key="fixture-key", api_key_header="X-Api-Key",
                         form={"taskid": "update_series"})
        self.assertEqual(result, {})
        self.assertEqual(seen, [("/api/system/tasks", "application/x-www-form-urlencoded", "fixture-key",
                                 b"taskid=update_series")])

    def test_http_failures_have_safe_categories_and_status(self):
        from http.server import BaseHTTPRequestHandler, HTTPServer
        import threading

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(401 if self.path == "/unauthorized" else 503)
                self.end_headers()
            def log_message(self, *_args):
                pass
        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        def close_server():
            server.shutdown()
            thread.join(timeout=2)
            server.server_close()
        self.addCleanup(close_server)
        base = "http://127.0.0.1:%d" % server.server_port
        for path, category, status in (("/unauthorized", "authentication", 401),
                                       ("/unavailable", "http_status", 503)):
            with self.subTest(path=path), self.assertRaises(HTTPError) as caught:
                request(base, path, api_key="do-not-leak")
            self.assertEqual(caught.exception.category, category)
            self.assertEqual(caught.exception.status_code, status)
            self.assertNotIn("do-not-leak", str(caught.exception))

    def test_invalid_json_and_response_limit_are_categorized(self):
        from http.server import BaseHTTPRequestHandler, HTTPServer
        import threading

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                payload = b"nope" if self.path == "/bad-json" else b"12345"
                self.send_response(200)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            def log_message(self, *_args):
                pass
        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        def close_server():
            server.shutdown()
            thread.join(timeout=2)
            server.server_close()
        self.addCleanup(close_server)
        base = "http://127.0.0.1:%d" % server.server_port
        for path, category in (("/bad-json", "invalid_json"), ("/large", "response_limit")):
            with self.subTest(path=path), self.assertRaises(HTTPError) as caught:
                request(base, path, max_bytes=4)
            self.assertEqual(caught.exception.category, category)

    def test_transport_failures_are_categorized_without_exception_details(self):
        import urllib.error
        from unittest.mock import patch
        cases = ((urllib.error.URLError(ConnectionRefusedError("secret URL")), "refused"),
                 (TimeoutError("secret URL"), "timeout"),
                 (urllib.error.URLError(OSError("network detail")), "unreachable"))
        for failure, category in cases:
            with self.subTest(category=category), \
                    patch("urllib.request.OpenerDirector.open", side_effect=failure), \
                    self.assertRaises(HTTPError) as caught:
                request("http://service.example", "/status", api_key="secret")
            self.assertEqual(caught.exception.category, category)
            self.assertNotIn("secret", str(caught.exception))
            self.assertNotIn("network detail", str(caught.exception))


if __name__ == "__main__":
    unittest.main()
