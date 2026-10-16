"""Disposable loopback fixture/load probe; run beside an isolated gateway only.

Configure all three gateway upstream URLs as http://127.0.0.1:9090,
JELLYFIN_API_KEY=test-key and GLANCE_LIVE_JELLYFIN_WS=false. This script
does not launch Docker, inspect the host, or read deployment configuration.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import http.client
import json
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seconds", type=int, default=10, choices=range(2, 61))
    args = parser.parse_args()
    lock = threading.Lock()
    calls = {"qbittorrent": 0, "jellyfin": 0}
    torrents = {
        f"test-{index:04}": {
            "name": f"Fixture episode {index:04}.mkv",
            "state": "downloading",
            "progress": 0.5,
            "dlspeed": 100,
            "upspeed": 2,
            "eta": 15,
        }
        for index in range(1000)
    }

    class Fixture(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_GET(self):
            target = urlsplit(self.path)
            if target.path.startswith("/api/pages/"):
                if self.headers.get("Cookie") != "auth=valid":
                    self.send_error(401)
                    return
                payload = b"fixture page"
            elif target.path == "/api/v2/sync/maindata":
                with lock:
                    calls["qbittorrent"] += 1
                full = parse_qs(target.query).get("rid", ["0"])[0] == "0"
                payload = json.dumps({
                    "rid": 1,
                    "full_update": full,
                    "server_state": {"dl_info_speed": 100000, "up_info_speed": 2000},
                    "torrents": torrents if full else {},
                }).encode()
            elif target.path == "/Sessions":
                if 'Token="test-key"' not in self.headers.get("Authorization", ""):
                    self.send_error(401)
                    return
                with lock:
                    calls["jellyfin"] += 1
                payload = json.dumps([{
                    "Id": "test-session", "UserName": "test-user",
                    "NowPlayingItem": {"Name": "Fixture film", "Type": "Movie", "RunTimeTicks": 1000},
                    "PlayState": {"PositionTicks": 100, "PlayMethod": "DirectStream"},
                }]).encode()
            elif target.path == "/fixture":
                payload = b"proxy fixture"
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(("127.0.0.1", 9090), Fixture)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    start = time.monotonic()

    def subscribe(index: int) -> int:
        page = "downloads" if index < 4 else "media"
        topic = "qbittorrent" if index < 4 else "jellyfin"
        conn = http.client.HTTPConnection("127.0.0.1", 8081, timeout=4)
        response = None
        try:
            conn.request("GET", f"/live/events?page={page}", headers={
                "Cookie": "auth=valid", "Origin": "http://127.0.0.1:8081",
            })
            response = conn.getresponse()
            assert response.status == 200, response.status
            assert response.getheader("Content-Type").startswith("text/event-stream")
            successes = 0
            while time.monotonic() - start < args.seconds:
                line = response.readline(1024 * 1024)
                assert line, "SSE stream closed early"
                if line.startswith(b"data: "):
                    event = json.loads(line[6:])
                    if event["status"] == "ok":
                        if topic == "qbittorrent":
                            assert len(event["data"]["torrents"]) == 1000
                        else:
                            assert len(event["data"]["sessions"]) == 1
                        successes += 1
            assert successes, f"no successful {topic} snapshot"
            return successes
        finally:
            if response is not None:
                response.close()
            conn.close()

    try:
        conn = http.client.HTTPConnection("127.0.0.1", 8081, timeout=3)
        conn.request("GET", "/fixture")
        assert conn.getresponse().read() == b"proxy fixture"
        conn.close()
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            snapshots = sum(pool.map(subscribe, range(8)))
        # Once each client closes, a shared collector must stop making requests.
        # Allow an in-flight request/connection-close detection to settle first.
        time.sleep(6)
        with lock:
            settled = dict(calls)
        time.sleep(2)
        with lock:
            assert calls == settled, f"collectors continued after subscribers left: {settled} -> {calls}"
        # Four clients per topic share one collector (two-second polling).
        assert max(calls.values()) <= args.seconds + 5, "duplicate collectors"
        print(json.dumps({"clients": 8, "torrents": 1000, "snapshots": snapshots,
                          "seconds": args.seconds, "upstream_calls": calls,
                          "collectors_stopped": True}))
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
