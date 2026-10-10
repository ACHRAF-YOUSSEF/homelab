"""Same-origin Glance gateway and live SSE relay."""
from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import math
import os
import time
from collections import defaultdict
from typing import Any
from urllib.parse import urlsplit, urlunsplit

from aiohttp import ClientError, ClientSession, ClientTimeout, CookieJar, DummyCookieJar, WSMsgType, web
from multidict import CIMultiDict

LOG = logging.getLogger("glance-live")
HOP_HEADERS = {"connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "te", "trailers", "transfer-encoding", "upgrade"}
PAGE_TOPICS = {"media": ("jellyfin",), "downloads": ("qbittorrent",)}
DEFAULT_TIMEOUT = ClientTimeout(total=15, connect=5, sock_read=10)
GLANCE_URL_KEY = web.AppKey("glance_url", str)
QB_URL_KEY = web.AppKey("qb_url", str)
JELLYFIN_URL_KEY = web.AppKey("jellyfin_url", str)
HTTP_CLIENT_KEY = web.AppKey("http", ClientSession)
RELAY_KEY = web.AppKey("relay", object)
ALLOWED_HOSTS_KEY = web.AppKey("allowed_hosts", frozenset)
QB_INTERVAL_KEY = web.AppKey("qb_interval", float)
JELLYFIN_INTERVAL_KEY = web.AppKey("jellyfin_interval", float)
JELLYFIN_WS_KEY = web.AppKey("jellyfin_ws", bool)
MAX_CONNECTIONS_KEY = web.AppKey("max_connections", int)
SSE_MAX_SECONDS_KEY = web.AppKey("sse_max_seconds", float)


async def reject_redirects(request, handler):
    response = await handler(request)
    if response.status in {301, 302, 303, 307, 308}:
        response.close()
        raise ClientError("redirect blocked")
    return response


def positive_float(name: str, default: float, maximum: float) -> float:
    raw = os.getenv(name, str(default))
    try:
        value = float(raw)
    except ValueError as exc:
        raise ValueError(f"{name} must be a finite positive number") from exc
    if not math.isfinite(value) or value <= 0 or value > maximum:
        raise ValueError(f"{name} must be a finite positive number no greater than {maximum:g}")
    return value


def parse_authority(authority: str) -> tuple[str, int | None]:
    if not authority or authority.strip() != authority or authority.endswith(":") or any(c.isspace() for c in authority) or any(c in authority for c in "\\/@?#"):
        raise ValueError("invalid host")
    parsed = urlsplit(f"//{authority}")
    if not parsed.hostname or parsed.username is not None or parsed.password is not None or parsed.path or parsed.query or parsed.fragment:
        raise ValueError("invalid host")
    try:
        port = parsed.port
    except ValueError as exc:
        raise ValueError("invalid host port") from exc
    if port == 0:
        raise ValueError("invalid host port")
    return parsed.hostname.lower().rstrip("."), port


@web.middleware
async def validate_host(request: web.Request, handler):
    try:
        hostname, _ = parse_authority(request.host)
    except ValueError:
        raise web.HTTPBadRequest(text="invalid host")
    if hostname not in request.app[ALLOWED_HOSTS_KEY]:
        raise web.HTTPMisdirectedRequest(text="host is not allowed")
    return await handler(request)


def validated_base(value: str, name: str) -> str:
    if not value or value.strip() != value or "\\" in value or any(c.isspace() for c in value):
        raise ValueError(f"{name} must be an absolute http(s) URL without credentials, query, or fragment")
    parsed = urlsplit(value)
    try:
        hostname, _port = parse_authority(parsed.netloc)
    except ValueError as exc:
        raise ValueError(f"{name} must be an absolute http(s) URL without credentials, query, or fragment") from exc
    if parsed.scheme not in {"http", "https"} or not hostname or parsed.query or parsed.fragment:
        raise ValueError(f"{name} must be an absolute http(s) URL without credentials, query, or fragment")
    return value.rstrip("/")


def compact_json(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


def normalize_session(item: dict[str, Any]) -> dict[str, Any] | None:
    now = item.get("NowPlayingItem")
    if not isinstance(now, dict):
        return None
    trans = item.get("TranscodingInfo")
    play_state = item.get("PlayState") or {}
    ticks = play_state.get("PositionTicks")
    rate = play_state.get("PlaybackRate") or 1
    return {
        "id": str(item.get("Id") or item.get("SessionId") or ""),
        "userName": item.get("UserName") or "",
        "title": now.get("Name") or "",
        "seriesName": now.get("SeriesName") or "",
        "type": now.get("Type") or "",
        "season": now.get("ParentIndexNumber"),
        "episode": now.get("IndexNumber"),
        "client": item.get("Client") or "",
        "deviceName": item.get("DeviceName") or "",
        "playMethod": play_state.get("PlayMethod") or ("Transcode" if trans else ""),
        "isPaused": bool(play_state.get("IsPaused", False)),
        "positionTicks": ticks if isinstance(ticks, (int, float)) else 0,
        "durationTicks": now.get("RunTimeTicks") if isinstance(now.get("RunTimeTicks"), (int, float)) else 0,
        "playbackRate": rate if isinstance(rate, (int, float)) else 1,
    }


def normalize_sessions(items: Any) -> list[dict[str, Any]]:
    normalized = []
    if isinstance(items, list):
        for item in items:
            if isinstance(item, dict):
                session = normalize_session(item)
                if session is not None:
                    normalized.append(session)
    return normalized


def normalize_torrents(items: dict[str, dict[str, Any]], server_state: dict[str, Any]) -> dict[str, Any]:
    # Keep the same state groupings used by qBittorrent's WebUI filters.
    dl_states = {"downloading", "metaDL", "forcedMetaDL", "forcedDL", "queuedDL", "stalledDL", "checkingDL", "pausedDL", "stoppedDL"}
    seeding_states = {"uploading", "forcedUP", "queuedUP", "stalledUP", "checkingUP"}
    torrents = []
    for key, value in items.items():
        state = str(value.get("state") or "")
        if state not in dl_states | seeding_states:
            continue
        torrents.append({
            "id": key,
            "name": str(value.get("name") or ""),
            "state": state,
            "progress": value.get("progress") if isinstance(value.get("progress"), (int, float)) else 0,
            "downloadSpeed": value.get("dlspeed") if isinstance(value.get("dlspeed"), (int, float)) else 0,
            "uploadSpeed": value.get("upspeed") if isinstance(value.get("upspeed"), (int, float)) else 0,
            "eta": value.get("eta") if isinstance(value.get("eta"), (int, float)) else 0,
        })
    return {
        "downloadSpeed": server_state.get("dl_info_speed", 0) or 0,
        "uploadSpeed": server_state.get("up_info_speed", 0) or 0,
        "seedingCount": sum(1 for value in items.values() if value.get("state") in seeding_states),
        "downloadingCount": sum(1 for value in items.values() if value.get("state") in dl_states),
        "torrents": [torrent for torrent in torrents if torrent["state"] in dl_states],
    }


def jellyfin_auth_header(api_key: str) -> str:
    return f'MediaBrowser Client="Glance Live", Device="Server", DeviceId="glance-live", Version="1.0", Token="{api_key}"'


class LiveRelay:
    def __init__(self, app: web.Application):
        self.app = app
        self.clients: dict[str, set[asyncio.Queue[dict[str, Any]]]] = defaultdict(set)
        self.snapshots: dict[str, dict[str, Any]] = {}
        self.tasks: dict[str, asyncio.Task[None]] = {}
        self.last_error: dict[str, bool] = defaultdict(bool)
        self.last_updated: dict[str, int | None] = defaultdict(lambda: None)
        self.qb_items: dict[str, dict[str, Any]] = {}
        self.qb_state: dict[str, Any] = {}
        self.qb_rid = 0
        self.stopping = False
        self.connection_count = 0
        self.last_source: dict[str, str] = {"jellyfin": "rest", "qbittorrent": "rest"}

    def event(self, topic: str) -> dict[str, Any]:
        updated = self.last_updated[topic]
        state = "warming" if updated is None and not self.last_error[topic] else "stale" if self.last_error[topic] else "ok"
        return {"status": state, "updatedAt": updated, "source": self.last_source[topic], "data": self.snapshots.get(topic)}

    def publish(self, topic: str, data: dict[str, Any], source: str | None = None) -> None:
        self.snapshots[topic] = data
        if source:
            self.last_source[topic] = source
        self.last_updated[topic] = int(time.time() * 1000)
        self.last_error[topic] = False
        for queue in tuple(self.clients[topic]):
            if queue.full():
                with contextlib.suppress(asyncio.QueueEmpty):
                    queue.get_nowait()
            with contextlib.suppress(asyncio.QueueFull):
                queue.put_nowait(self.event(topic))

    def fail(self, topic: str) -> None:
        self.last_error[topic] = True
        for queue in tuple(self.clients[topic]):
            if queue.full():
                with contextlib.suppress(asyncio.QueueEmpty):
                    queue.get_nowait()
            with contextlib.suppress(asyncio.QueueFull):
                queue.put_nowait(self.event(topic))

    async def subscribe(self, topic: str):
        if self.connection_count >= self.app[MAX_CONNECTIONS_KEY]:
            return None
        queue: asyncio.Queue[dict[str, Any]] = asyncio.Queue(maxsize=2)
        self.connection_count += 1
        self.clients[topic].add(queue)
        if topic not in self.tasks:
            # A cached snapshot is unverified until the new collector succeeds.
            self.last_error[topic] = self.last_updated[topic] is not None
            self.tasks[topic] = asyncio.create_task(self.collect(topic), name=f"collector-{topic}")
        return queue

    async def unsubscribe(self, topic: str, queue: asyncio.Queue[dict[str, Any]]) -> None:
        if queue in self.clients[topic]:
            self.clients[topic].discard(queue)
            self.connection_count = max(0, self.connection_count - 1)
        if not self.clients[topic]:
            task = self.tasks.pop(topic, None)
            if task:
                task.cancel()
                with contextlib.suppress(asyncio.CancelledError):
                    await task

    async def collect(self, topic: str) -> None:
        try:
            if topic == "jellyfin":
                await self.collect_jellyfin()
            else:
                await self.collect_qb()
        except asyncio.CancelledError:
            raise
        except Exception:
            LOG.warning("collector failed provider=%s category=unexpected", topic)
            self.fail(topic)

    async def collect_qb(self) -> None:
        base = self.app[QB_URL_KEY]
        username, password = os.getenv("QBITTORRENT_USERNAME", ""), os.getenv("QBITTORRENT_PASSWORD", "")
        delay = self.app[QB_INTERVAL_KEY]
        session = ClientSession(timeout=DEFAULT_TIMEOUT, cookie_jar=CookieJar(unsafe=True))
        authenticated = not (username or password)
        try:
            while self.clients["qbittorrent"] and not self.stopping:
                try:
                    if not authenticated:
                        if not username or not password:
                            self.fail("qbittorrent")
                            await asyncio.sleep(min(delay, 5))
                            continue
                        async with session.post(f"{base}/api/v2/auth/login", data={"username": username, "password": password}, allow_redirects=False) as response:
                            logged_in = response.status == 200 and (await response.text()) == "Ok."
                        if not logged_in:
                            LOG.warning("collector failed provider=qbittorrent category=authentication")
                            self.fail("qbittorrent")
                            await asyncio.sleep(min(delay, 5))
                            continue
                        authenticated = True
                        self.qb_rid = 0
                        self.qb_items = {}
                        self.qb_state = {}
                    async with session.get(f"{base}/api/v2/sync/maindata", params={"rid": self.qb_rid}, allow_redirects=False) as response:
                        status = response.status
                        payload = await response.json() if status == 200 else None
                    if status in {401, 403} and (username or password):
                        authenticated = False
                        self.qb_rid = 0
                        self.qb_items = {}
                        self.qb_state = {}
                        self.fail("qbittorrent")
                        await asyncio.sleep(min(delay, 5))
                        continue
                    if status != 200:
                        LOG.warning("collector failed provider=qbittorrent category=response")
                        self.fail("qbittorrent")
                        await asyncio.sleep(min(delay, 5))
                        continue
                    if not isinstance(payload, dict):
                        raise ValueError("invalid response")
                    if payload.get("full_update"):
                        self.qb_items = dict(payload.get("torrents") or {})
                    else:
                        for torrent_id, changed_fields in (payload.get("torrents") or {}).items():
                            existing = self.qb_items.get(torrent_id, {})
                            self.qb_items[torrent_id] = {**existing, **changed_fields} if isinstance(changed_fields, dict) else existing
                        for removed in payload.get("torrents_removed") or []:
                            self.qb_items.pop(str(removed), None)
                    if payload.get("full_update"):
                        self.qb_state = dict(payload.get("server_state") or {})
                    else:
                        self.qb_state.update(payload.get("server_state") or {})
                    self.qb_rid = payload.get("rid", self.qb_rid)
                    self.publish("qbittorrent", normalize_torrents(self.qb_items, self.qb_state))
                except asyncio.CancelledError:
                    raise
                except Exception:
                    LOG.warning("collector failed provider=qbittorrent category=request")
                    self.fail("qbittorrent")
                await asyncio.sleep(max(0.5, min(delay, 30)))
        finally:
            await session.close()

    async def collect_jellyfin(self) -> None:
        base = self.app[JELLYFIN_URL_KEY]
        api_key = os.getenv("JELLYFIN_API_KEY", "")
        if not api_key:
            self.fail("jellyfin")
            return
        parsed = urlsplit(base)
        ws_scheme = "wss" if parsed.scheme == "https" else "ws"
        ws_url = urlunsplit((ws_scheme, parsed.netloc, f"{parsed.path.rstrip('/')}/socket", "", ""))
        headers = {"Authorization": jellyfin_auth_header(api_key)}
        timeout = self.app[JELLYFIN_INTERVAL_KEY]
        websocket_enabled = self.app[JELLYFIN_WS_KEY]
        session = ClientSession(timeout=DEFAULT_TIMEOUT, cookie_jar=DummyCookieJar(), middlewares=(reject_redirects,))
        try:
            while self.clients["jellyfin"] and not self.stopping:
                # REST is the durable fallback; a failed optional WebSocket must
                # not make a recent REST snapshot appear stale.
                try:
                    await self.reconcile_jellyfin(session, base, headers)
                except asyncio.CancelledError:
                    raise
                except Exception:
                    LOG.warning("collector failed provider=jellyfin category=rest")
                    self.fail("jellyfin")
                if not websocket_enabled:
                    await asyncio.sleep(max(1, min(timeout, 60)))
                    continue
                try:
                    async with session.ws_connect(ws_url, headers=headers, heartbeat=25, receive_timeout=None, max_msg_size=1_000_000) as ws:
                        await ws.send_json({"MessageType": "SessionsStart", "Data": "0,2000"})
                        last_reconcile = time.monotonic()
                        while self.clients["jellyfin"] and not self.stopping:
                            try:
                                message = await asyncio.wait_for(ws.receive(), timeout=max(2, timeout))
                            except asyncio.TimeoutError:
                                message = None
                            if time.monotonic() - last_reconcile >= timeout:
                                try:
                                    await self.reconcile_jellyfin(session, base, headers)
                                except asyncio.CancelledError:
                                    raise
                                except Exception:
                                    LOG.warning("collector failed provider=jellyfin category=reconcile")
                                    self.fail("jellyfin")
                                last_reconcile = time.monotonic()
                            if message is None:
                                continue
                            if message.type == WSMsgType.TEXT:
                                try:
                                    parsed_message = json.loads(message.data)
                                except (ValueError, TypeError):
                                    continue
                                kind = parsed_message.get("MessageType")
                                if kind == "Sessions" and isinstance(parsed_message.get("Data"), list):
                                    self.publish("jellyfin", {"sessions": normalize_sessions(parsed_message["Data"])}, "websocket")
                                elif kind == "KeepAlive":
                                    continue
                            elif message.type in {WSMsgType.ERROR, WSMsgType.CLOSED, WSMsgType.CLOSE}:
                                break
                except asyncio.CancelledError:
                    raise
                except Exception:
                    LOG.warning("collector failed provider=jellyfin category=websocket")
                if self.clients["jellyfin"] and not self.stopping:
                    await asyncio.sleep(max(1, min(timeout, 60)))
        finally:
            await session.close()

    async def reconcile_jellyfin(self, session: ClientSession, base: str, headers: dict[str, str]) -> None:
        async with session.get(f"{base}/Sessions", headers=headers, allow_redirects=False) as response:
            if response.status != 200:
                raise RuntimeError("reconciliation status")
            payload = await response.json()
        self.publish("jellyfin", {"sessions": normalize_sessions(payload)}, "rest")


async def authorize_page(request: web.Request, page: str) -> bool:
    cookie = request.headers.get("Cookie")
    url = f"{request.app[GLANCE_URL_KEY]}/api/pages/{page}/content/"
    try:
        headers = {"Cookie": cookie} if cookie else {}
        async with request.app[HTTP_CLIENT_KEY].get(url, headers=headers, allow_redirects=False) as response:
            return response.status == 200
    except (ClientError, asyncio.TimeoutError):
        return False


async def live_events(request: web.Request) -> web.StreamResponse:
    page = request.query.get("page")
    if page not in PAGE_TOPICS:
        raise web.HTTPBadRequest(text="page must be media or downloads")
    origin = request.headers.get("Origin")
    if origin:
        parsed_origin = urlsplit(origin)
        try:
            origin_host, origin_port = parse_authority(parsed_origin.netloc)
            request_host, request_port = parse_authority(request.host)
        except ValueError:
            raise web.HTTPForbidden()
        if parsed_origin.scheme not in {"http", "https"} or parsed_origin.path or parsed_origin.query or parsed_origin.fragment or parsed_origin.username or parsed_origin.password or (origin_host, origin_port) != (request_host, request_port):
            raise web.HTTPForbidden()
    if request.headers.get("Sec-Fetch-Site", "").lower() in {"cross-site", "same-site"}:
        raise web.HTTPForbidden()
    if not await authorize_page(request, page):
        raise web.HTTPUnauthorized()
    relay: LiveRelay = request.app[RELAY_KEY]
    topic = PAGE_TOPICS[page][0]
    queue = await relay.subscribe(topic)
    if queue is None:
        raise web.HTTPServiceUnavailable(text="live connection limit reached")
    response = web.StreamResponse(status=200, headers={"Content-Type": "text/event-stream", "Cache-Control": "no-cache, no-transform", "X-Accel-Buffering": "no", "Connection": "keep-alive"})
    max_lifetime = request.app[SSE_MAX_SECONDS_KEY]
    write_timeout = 10.0
    started = time.monotonic()
    try:
        await asyncio.wait_for(response.prepare(request), timeout=write_timeout)
        await asyncio.wait_for(response.write(f"event: {topic}\ndata: {compact_json(relay.event(topic))}\n\n".encode()), timeout=write_timeout)
        while True:
            remaining = max_lifetime - (time.monotonic() - started)
            if remaining <= 0:
                break
            try:
                payload = await asyncio.wait_for(queue.get(), timeout=min(20, remaining))
            except asyncio.TimeoutError:
                if remaining <= 20:
                    break
                await asyncio.wait_for(response.write(b": keepalive\n\n"), timeout=write_timeout)
                continue
            await asyncio.wait_for(response.write(f"event: {topic}\ndata: {compact_json(payload)}\n\n".encode()), timeout=write_timeout)
    except (ConnectionResetError, asyncio.CancelledError, asyncio.TimeoutError):
        pass
    finally:
        await relay.unsubscribe(topic, queue)
    return response


async def health(_: web.Request) -> web.Response:
    return web.json_response({"status": "ok"})


async def proxy(request: web.Request) -> web.StreamResponse:
    if request.headers.get("Upgrade", "").lower() == "websocket":
        raise web.HTTPNotImplemented(text="WebSocket upgrades are not supported by this gateway")
    if request.method not in {"GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"}:
        raise web.HTTPMethodNotAllowed(request.method, {"GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"})
    if request.path.startswith("/live/"):
        raise web.HTTPNotFound()
    upstream = request.app[GLANCE_URL_KEY] + request.rel_url.path_qs
    headers = {k: v for k, v in request.headers.items() if k.lower() not in HOP_HEADERS and k.lower() not in {"host", "content-length", "forwarded", "x-forwarded-for", "x-forwarded-host", "x-forwarded-proto", "x-forwarded-uri"}}
    headers["X-Forwarded-Host"] = request.host
    headers["X-Forwarded-Proto"] = request.scheme
    headers["X-Forwarded-For"] = request.remote or "unknown"
    headers["X-Forwarded-Uri"] = request.rel_url.path_qs
    if request.content_length is not None and request.can_read_body:
        headers["Content-Length"] = str(request.content_length)
    try:
        async with request.app[HTTP_CLIENT_KEY].request(request.method, upstream, headers=headers, data=request.content if request.can_read_body else None, allow_redirects=False) as remote:
            out_headers = CIMultiDict()
            for key, value in remote.headers.items():
                if key.lower() not in HOP_HEADERS and key.lower() not in {"content-length", "set-cookie"}:
                    out_headers.add(key, value)
            for cookie in remote.headers.getall("Set-Cookie", []):
                out_headers.add("Set-Cookie", cookie)
            response = web.StreamResponse(status=remote.status, reason=remote.reason, headers=out_headers)
            await response.prepare(request)
            async for chunk in remote.content.iter_chunked(64 * 1024):
                await response.write(chunk)
            await response.write_eof()
            return response
    except (ClientError, asyncio.TimeoutError):
        raise web.HTTPBadGateway(text="Glance upstream unavailable")


async def on_startup(app: web.Application) -> None:
    app[HTTP_CLIENT_KEY] = ClientSession(timeout=DEFAULT_TIMEOUT, cookie_jar=DummyCookieJar(), auto_decompress=False)
    app[RELAY_KEY] = LiveRelay(app)


async def on_cleanup(app: web.Application) -> None:
    relay: LiveRelay = app[RELAY_KEY]
    relay.stopping = True
    for task in relay.tasks.values():
        task.cancel()
    await asyncio.gather(*relay.tasks.values(), return_exceptions=True)
    await app[HTTP_CLIENT_KEY].close()


def create_app() -> web.Application:
    app = web.Application(client_max_size=8 * 1024 * 1024, middlewares=[validate_host])
    app[GLANCE_URL_KEY] = validated_base(os.getenv("GLANCE_UPSTREAM_URL", "http://glance:8080"), "GLANCE_UPSTREAM_URL")
    app[QB_URL_KEY] = validated_base(os.getenv("QBITTORRENT_INTERNAL_URL", "http://qbittorrent:8080"), "QBITTORRENT_INTERNAL_URL")
    jellyfin = os.getenv("JELLYFIN_INTERNAL_URL", "http://jellyfin:8096")
    app[JELLYFIN_URL_KEY] = validated_base(jellyfin, "JELLYFIN_INTERNAL_URL")
    allowed_hosts = os.getenv("GLANCE_LIVE_ALLOWED_HOSTS", "localhost,127.0.0.1").split(",")
    parsed_hosts = {parse_authority(host.strip())[0] for host in allowed_hosts if host.strip()}
    if not parsed_hosts:
        raise ValueError("GLANCE_LIVE_ALLOWED_HOSTS must contain at least one host")
    app[ALLOWED_HOSTS_KEY] = frozenset(parsed_hosts)
    app[QB_INTERVAL_KEY] = positive_float("GLANCE_LIVE_QBITTORRENT_INTERVAL", 2, 60)
    app[JELLYFIN_INTERVAL_KEY] = positive_float("GLANCE_LIVE_JELLYFIN_INTERVAL", 5, 60)
    ws_value = os.getenv("GLANCE_LIVE_JELLYFIN_WS", "true").strip().lower()
    if ws_value not in {"1", "true", "yes", "on", "0", "false", "no", "off"}:
        raise ValueError("GLANCE_LIVE_JELLYFIN_WS must be a boolean")
    app[JELLYFIN_WS_KEY] = ws_value in {"1", "true", "yes", "on"}
    max_connections = os.getenv("GLANCE_LIVE_MAX_CONNECTIONS", "64")
    if not max_connections.isdigit() or not 1 <= int(max_connections) <= 1000:
        raise ValueError("GLANCE_LIVE_MAX_CONNECTIONS must be an integer from 1 to 1000")
    app[MAX_CONNECTIONS_KEY] = int(max_connections)
    app[SSE_MAX_SECONDS_KEY] = positive_float("GLANCE_LIVE_SSE_MAX_SECONDS", 900, 86400)
    app[RELAY_KEY] = LiveRelay(app)
    app.router.add_get("/live/health", health)
    app.router.add_get("/live/events", live_events)
    app.router.add_route("*", "/{tail:.*}", proxy)
    app.on_startup.append(on_startup)
    app.on_cleanup.append(on_cleanup)
    return app


if __name__ == "__main__":
    logging.basicConfig(level=os.getenv("LOG_LEVEL", "INFO"), format="%(levelname)s %(name)s %(message)s")
    web.run_app(create_app(), host=os.getenv("GLANCE_LIVE_BIND_ADDRESS", "127.0.0.1"), port=int(os.getenv("GLANCE_LIVE_PORT", "8081")), access_log=None)
