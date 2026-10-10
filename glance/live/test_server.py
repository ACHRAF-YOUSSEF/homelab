"""Disposable HTTP/WebSocket integration tests for the live gateway."""
from __future__ import annotations

import asyncio
import json
import os
import unittest
from unittest.mock import patch

from aiohttp import WSMsgType, web
from aiohttp.test_utils import TestClient, TestServer

from server import JELLYFIN_INTERVAL_KEY, JELLYFIN_WS_KEY, RELAY_KEY, LiveRelay, create_app, normalize_torrents, validated_base


class GatewayTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.qb_calls = 0
        self.qb_fail = False
        self.qb_force_full = False
        self.qb_requires_auth = False
        self.qb_login_failures = 0
        self.qb_login_calls = 0
        self.qb_login_status = 200
        self.qb_login_body = "Ok."
        self.qb_login_redirect = False
        self.qb_expire_once = False
        self.redirect_websocket = False
        self.leak_calls = 0
        self.jelly_sessions_calls = 0
        self.ws_attempted = asyncio.Event()
        self.ws_connected = asyncio.Event()
        self.remote = web.Application()

        async def glance_page(request):
            if request.headers.get("Cookie") != "auth=valid":
                return web.Response(status=401)
            return web.Response(text="rendered page")

        async def qb_sync(request):
            self.qb_calls += 1
            if self.qb_requires_auth:
                if request.cookies.get("SID") != "valid":
                    return web.Response(status=401)
                if self.qb_expire_once:
                    self.qb_expire_once = False
                    return web.Response(status=401)
            if self.qb_fail:
                return web.Response(status=503)
            if self.qb_force_full:
                return web.json_response({
                    "rid": 10,
                    "full_update": True,
                    "server_state": {"dl_info_speed": 5},
                    "torrents": {"new": {"name": "new.mkv", "state": "downloading", "progress": 0.1}},
                })
            if request.query.get("rid") == "0":
                return web.json_response({
                    "rid": 1,
                    "full_update": True,
                    "server_state": {"dl_info_speed": 123, "up_info_speed": 7},
                    "torrents": {
                        "hash-a": {"name": "episode.mkv", "state": "downloading", "progress": 0.5, "dlspeed": 100, "upspeed": 2, "eta": 15},
                        "hash-seed": {"name": "seed.mkv", "state": "uploading", "progress": 1},
                    },
                })
            return web.json_response({
                "rid": 2,
                "server_state": {"up_info_speed": 9},
                "torrents": {"hash-a": {"state": "pausedDL"}},
                "torrents_removed": [],
            })

        async def qb_login(request):
            self.qb_login_calls += 1
            form = await request.post()
            self.assertEqual(form.get("username"), "fixture-user")
            self.assertEqual(form.get("password"), "fixture-pass")
            if self.qb_login_redirect:
                return web.Response(status=307, headers={"Location": str(self.leak_server.make_url("/leak"))})
            if self.qb_login_failures:
                self.qb_login_failures -= 1
                return web.Response(text="Fails.")
            if self.qb_login_status == 204:
                return web.Response(status=204, headers={"Set-Cookie": "SID=valid; Path=/"})
            return web.Response(status=self.qb_login_status, text=self.qb_login_body, headers={"Set-Cookie": "SID=valid; Path=/"})

        async def proxy_target(_request):
            response = web.Response(text="proxy-body")
            response.headers.add("Set-Cookie", "one=1; Path=/")
            response.headers.add("Set-Cookie", "two=2; Path=/")
            response.headers["X-Origin"] = "fixture"
            return response

        async def jelly_sessions(request):
            self.jelly_sessions_calls += 1
            self.assertIn('Token="fixture-key"', request.headers.get("Authorization", ""))
            return web.json_response([
                {"Id": "idle", "UserName": "listener"},
                {"Id": "rest", "UserName": "fixture", "NowPlayingItem": {"Name": "Rest title", "Type": "Episode", "RunTimeTicks": 1000}, "PlayState": {"PositionTicks": 20, "PlayMethod": "DirectStream"}},
            ])

        async def jelly_socket(request):
            self.ws_attempted.set()
            self.assertNotIn("api_key", request.query)
            self.assertIn('Token="fixture-key"', request.headers.get("Authorization", ""))
            if self.redirect_websocket:
                return web.Response(status=302, headers={"Location": str(self.leak_server.make_url("/leak"))})
            self.ws_connected.set()
            ws = web.WebSocketResponse()
            await ws.prepare(request)
            async for message in ws:
                if message.type == WSMsgType.TEXT:
                    cmd = json.loads(message.data)
                    if cmd.get("MessageType") == "SessionsStart":
                        await ws.send_json({"MessageType": "Sessions", "Data": [
                            {"Id": "ws-idle", "UserName": "listener"},
                            {"Id": "ws", "UserName": "listener", "NowPlayingItem": {"Name": "Film", "Type": "Movie", "RunTimeTicks": 1000}, "PlayState": {"PositionTicks": 100, "IsPaused": False, "PlayMethod": "DirectStream"}},
                        ]})
            return ws

        async def leak_target(_request):
            self.leak_calls += 1
            return web.Response(text="received")

        self.remote.router.add_get("/api/pages/{page}/content/", glance_page)
        self.remote.router.add_get("/api/v2/sync/maindata", qb_sync)
        self.remote.router.add_post("/api/v2/auth/login", qb_login)
        self.remote.router.add_get("/plain", proxy_target)
        self.remote.router.add_get("/Sessions", jelly_sessions)
        self.remote.router.add_get("/socket", jelly_socket)
        leak_app = web.Application()
        leak_app.router.add_route("*", "/leak", leak_target)
        self.leak_server = TestServer(leak_app)
        await self.leak_server.start_server()
        self.remote_server = TestServer(self.remote)
        await self.remote_server.start_server()
        self.patch_env = patch.dict(os.environ, {
            "GLANCE_UPSTREAM_URL": str(self.remote_server.make_url("/" )).rstrip("/"),
            "QBITTORRENT_INTERNAL_URL": str(self.remote_server.make_url("/" )).rstrip("/"),
            "JELLYFIN_INTERNAL_URL": str(self.remote_server.make_url("/" )).rstrip("/"),
            "JELLYFIN_API_KEY": "fixture-key",
            "QBITTORRENT_USERNAME": "",
            "QBITTORRENT_PASSWORD": "",
            "GLANCE_LIVE_QBITTORRENT_INTERVAL": "0.5",
            "GLANCE_LIVE_JELLYFIN_INTERVAL": "2",
        })
        self.patch_env.start()
        self.client = TestClient(TestServer(create_app()))
        await self.client.start_server()

    async def asyncTearDown(self):
        await self.client.close()
        await self.remote_server.close()
        await self.leak_server.close()
        self.patch_env.stop()

    async def test_health_is_public_and_generic(self):
        response = await self.client.get("/live/health")
        self.assertEqual(response.status, 200)
        self.assertEqual(await response.json(), {"status": "ok"})

    async def test_initial_snapshot_is_warming_with_no_fabricated_data(self):
        event = self.client.server.app[RELAY_KEY].event("jellyfin")
        self.assertEqual(event, {"status": "warming", "updatedAt": None, "source": "rest", "data": None})

    async def test_unapproved_host_is_rejected(self):
        response = await self.client.get("/live/health", headers={"Host": "attacker.example"})
        self.assertEqual(response.status, 421)

    async def test_sse_requires_glance_page_access_and_same_origin(self):
        denied = await self.client.get("/live/events?page=downloads")
        self.assertEqual(denied.status, 401)
        cross = await self.client.get("/live/events?page=downloads", headers={"Cookie": "auth=valid", "Origin": "https://elsewhere.example"})
        self.assertEqual(cross.status, 403)
        response = await self.client.get("/live/events?page=downloads", headers={"Cookie": "auth=valid", "Origin": str(self.client.make_url("/" )).rstrip("/")})
        self.assertEqual(response.status, 200)
        self.assertIn(b"event: qbittorrent", await asyncio.wait_for(response.content.readline(), timeout=1))
        response.close()
        for _ in range(20):
            if self.client.server.app[RELAY_KEY].connection_count == 0:
                break
            await asyncio.sleep(0.01)
        self.assertEqual(self.client.server.app[RELAY_KEY].connection_count, 0)

    async def test_qbittorrent_incremental_state_and_cleanup(self):
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        queue = await relay.subscribe("qbittorrent")
        await asyncio.wait_for(queue.get(), timeout=2)
        self.assertEqual(relay.snapshots["qbittorrent"]["downloadSpeed"], 123)
        await asyncio.wait_for(queue.get(), timeout=2)
        snapshot = relay.snapshots["qbittorrent"]
        self.assertEqual(snapshot["torrents"][0]["state"], "pausedDL")
        self.assertEqual(snapshot["torrents"][0]["name"], "episode.mkv")
        self.assertEqual(snapshot["downloadingCount"], 1)
        self.assertEqual(snapshot["seedingCount"], 1)
        self.assertEqual(snapshot["uploadSpeed"], 9)
        self.qb_force_full = True
        deadline = asyncio.get_running_loop().time() + 3
        while asyncio.get_running_loop().time() < deadline:
            await asyncio.wait_for(queue.get(), timeout=2)
            if [item["id"] for item in relay.snapshots["qbittorrent"]["torrents"]] == ["new"]:
                break
        full = relay.snapshots["qbittorrent"]
        self.assertEqual(full["uploadSpeed"], 0)
        self.assertEqual([item["id"] for item in full["torrents"]], ["new"])
        await relay.unsubscribe("qbittorrent", queue)
        self.assertNotIn("qbittorrent", relay.tasks)

    async def test_jellyfin_websocket_snapshot_and_rest_fallback(self):
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        queue = await relay.subscribe("jellyfin")
        await asyncio.wait_for(self.ws_connected.wait(), timeout=2)
        snapshots = [await asyncio.wait_for(queue.get(), timeout=2) for _ in range(2)]
        self.assertEqual([x["id"] for x in relay.snapshots["jellyfin"]["sessions"]], ["ws"])
        self.assertEqual(relay.snapshots["jellyfin"]["sessions"][0]["playMethod"], "DirectStream")
        self.assertIn(snapshots[-1]["source"], {"rest", "websocket"})
        await relay.unsubscribe("jellyfin", queue)
        self.assertNotIn("jellyfin", relay.tasks)

    async def test_jellyfin_reconciles_rest_during_a_quiet_websocket(self):
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        queue = await relay.subscribe("jellyfin")
        await asyncio.wait_for(self.ws_connected.wait(), timeout=2)
        await asyncio.wait_for(queue.get(), timeout=2)  # initial REST snapshot
        await asyncio.wait_for(queue.get(), timeout=2)  # WebSocket snapshot
        reconciled = await asyncio.wait_for(queue.get(), timeout=3)
        self.assertEqual(reconciled["source"], "rest")
        self.assertEqual(relay.snapshots["jellyfin"]["sessions"][0]["id"], "rest")
        await relay.unsubscribe("jellyfin", queue)

    async def test_jellyfin_blocks_redirect_without_disclosing_token(self):
        self.redirect_websocket = True
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        queue = await relay.subscribe("jellyfin")
        await asyncio.wait_for(self.ws_attempted.wait(), timeout=2)
        await asyncio.wait_for(queue.get(), timeout=2)  # initial REST snapshot
        deadline = asyncio.get_running_loop().time() + 5
        while asyncio.get_running_loop().time() < deadline and self.jelly_sessions_calls < 2:
            await asyncio.sleep(0.05)
        self.assertEqual(self.leak_calls, 0)
        self.assertFalse(self.ws_connected.is_set())
        self.assertGreaterEqual(self.jelly_sessions_calls, 2)
        self.assertEqual(relay.event("jellyfin")["status"], "ok")
        self.assertEqual(relay.event("jellyfin")["source"], "rest")
        await relay.unsubscribe("jellyfin", queue)

    async def test_new_collector_marks_cached_snapshot_stale_until_refresh(self):
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        first = await relay.subscribe("qbittorrent")
        await asyncio.wait_for(first.get(), timeout=2)
        self.assertEqual(relay.event("qbittorrent")["status"], "ok")
        await relay.unsubscribe("qbittorrent", first)
        second = await relay.subscribe("qbittorrent")
        self.assertEqual(relay.event("qbittorrent")["status"], "stale")
        refreshed = await asyncio.wait_for(second.get(), timeout=2)
        self.assertEqual(refreshed["status"], "ok")
        await relay.unsubscribe("qbittorrent", second)

    async def test_jellyfin_uses_rest_polling_when_websocket_is_disabled(self):
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        self.client.server.app[JELLYFIN_WS_KEY] = False
        self.client.server.app[JELLYFIN_INTERVAL_KEY] = 1
        queue = await relay.subscribe("jellyfin")
        first = await asyncio.wait_for(queue.get(), timeout=2)
        second = await asyncio.wait_for(queue.get(), timeout=2)
        self.assertEqual(first["source"], "rest")
        self.assertEqual(second["source"], "rest")
        self.assertFalse(self.ws_connected.is_set())
        await relay.unsubscribe("jellyfin", queue)

    async def test_qbittorrent_reports_stale_after_upstream_failure(self):
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        queue = await relay.subscribe("qbittorrent")
        await asyncio.wait_for(queue.get(), timeout=2)
        fresh = relay.event("qbittorrent")
        self.qb_fail = True
        stale = await asyncio.wait_for(queue.get(), timeout=2)
        self.assertEqual(stale["status"], "stale")
        self.assertEqual(stale["updatedAt"], fresh["updatedAt"])
        await relay.unsubscribe("qbittorrent", queue)

    async def test_qbittorrent_retries_login_and_reauthenticates_expired_sid(self):
        self.qb_requires_auth = True
        self.qb_login_failures = 1
        self.qb_expire_once = True
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        with patch.dict(os.environ, {"QBITTORRENT_USERNAME": "fixture-user", "QBITTORRENT_PASSWORD": "fixture-pass"}):
            queue = await relay.subscribe("qbittorrent")
            deadline = asyncio.get_running_loop().time() + 5
            while asyncio.get_running_loop().time() < deadline:
                if self.qb_login_calls >= 3 and relay.last_updated["qbittorrent"] is not None:
                    break
                await asyncio.sleep(0.05)
            self.assertGreaterEqual(self.qb_login_calls, 3)
            self.assertEqual(relay.event("qbittorrent")["status"], "ok")
            await relay.unsubscribe("qbittorrent", queue)

    async def test_qbittorrent_204_login_and_expired_session_reauthentication(self):
        self.qb_requires_auth = True
        self.qb_login_status = 204
        self.qb_expire_once = True
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        with patch.dict(os.environ, {"QBITTORRENT_USERNAME": "fixture-user", "QBITTORRENT_PASSWORD": "fixture-pass"}):
            queue = await relay.subscribe("qbittorrent")
            try:
                deadline = asyncio.get_running_loop().time() + 3
                while asyncio.get_running_loop().time() < deadline:
                    event = await asyncio.wait_for(queue.get(), timeout=2)
                    if event["status"] == "ok":
                        break
                self.assertEqual(relay.event("qbittorrent")["status"], "ok")
                self.assertEqual(self.qb_login_calls, 2)
                self.assertEqual(relay.snapshots["qbittorrent"]["downloadSpeed"], 123)
            finally:
                await relay.unsubscribe("qbittorrent", queue)

    async def test_qbittorrent_rejected_login_does_not_fetch_or_publish_data(self):
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        with patch.dict(os.environ, {"QBITTORRENT_USERNAME": "fixture-user", "QBITTORRENT_PASSWORD": "fixture-pass"}):
            for status, body in ((200, "Fails."), (200, ""), (200, "unexpected"), (401, ""), (403, "")):
                with self.subTest(status=status, body=body):
                    self.qb_login_status, self.qb_login_body = status, body
                    queue = await relay.subscribe("qbittorrent")
                    try:
                        event = await asyncio.wait_for(queue.get(), timeout=2)
                        self.assertEqual(event["status"], "stale")
                        self.assertIsNone(event["data"])
                        self.assertEqual(self.qb_calls, 0)
                    finally:
                        await relay.unsubscribe("qbittorrent", queue)

    async def test_qbittorrent_login_redirect_does_not_forward_credentials(self):
        self.qb_login_redirect = True
        relay: LiveRelay = self.client.server.app[RELAY_KEY]
        with patch.dict(os.environ, {"QBITTORRENT_USERNAME": "fixture-user", "QBITTORRENT_PASSWORD": "fixture-pass"}):
            queue = await relay.subscribe("qbittorrent")
            try:
                event = await asyncio.wait_for(queue.get(), timeout=2)
                self.assertEqual(event["status"], "stale")
                self.assertEqual(self.leak_calls, 0)
                self.assertEqual(self.qb_calls, 0)
            finally:
                await relay.unsubscribe("qbittorrent", queue)

    async def test_proxy_preserves_multiple_set_cookie_and_blocks_upgrades(self):
        response = await self.client.get("/plain")
        self.assertEqual(await response.text(), "proxy-body")
        self.assertEqual(response.headers.getall("Set-Cookie"), ["one=1; Path=/", "two=2; Path=/"])
        upgrade = await self.client.get("/plain", headers={"Upgrade": "websocket", "Connection": "Upgrade"})
        self.assertEqual(upgrade.status, 501)
        unknown_live = await self.client.get("/live/arbitrary")
        self.assertEqual(unknown_live.status, 404)


class NormalizationTests(unittest.TestCase):
    def test_paused_download_filter_and_removal_semantics(self):
        data = normalize_torrents({
            "a": {"name": "a", "state": "pausedDL", "progress": 1},
            "b": {"name": "b", "state": "pausedUP", "progress": 1},
            "c": {"name": "c", "state": "error", "progress": 0},
            "d": {"name": "d", "state": "forcedMetaDL", "progress": 0.2},
            "e": {"name": "e", "state": "checkingResumeData", "progress": 0},
            "f": {"name": "f", "state": "stoppedUP", "progress": 1},
            "g": {"name": "g", "state": "checkingUP", "progress": 1},
        }, {"dl_info_speed": 50, "up_info_speed": 10})
        self.assertEqual([x["id"] for x in data["torrents"]], ["a", "d"])
        self.assertEqual(data["downloadingCount"], 2)
        self.assertEqual(data["seedingCount"], 1)
        self.assertEqual(data["downloadSpeed"], 50)

    def test_upstream_url_validation_rejects_ambiguous_authorities(self):
        for value in ("http://@localhost", "http://localhost:bad", "http://localhost:", "http://bad host", "http://localhost\\@evil"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                validated_base(value, "TEST_URL")


if __name__ == "__main__":
    unittest.main()
