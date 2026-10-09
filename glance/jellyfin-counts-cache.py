"""Serve the last successful Jellyfin library counts to Glance."""

import json
import logging
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.request import Request

# Direct repository execution shares the same client that Compose mounts in /app.
client_directory = Path(__file__).resolve().parents[1] / "subtitle-worker"
if client_directory.is_dir():
    sys.path.insert(0, str(client_directory))
from http_client import open_http


JELLYFIN_URL = os.environ["JELLYFIN_INTERNAL_URL"].rstrip("/") + "/Items/Counts"
TOKEN = os.environ["JELLYFIN_API_KEY"]
BIND_ADDRESS = os.getenv("JELLYFIN_COUNTS_BIND_ADDRESS", "").strip() or "127.0.0.1"
COUNTS = None
LOCK = threading.Lock()


def refresh_counts():
    global COUNTS
    while True:
        try:
            request = Request(
                JELLYFIN_URL,
                headers={
                    "Authorization": (
                        'MediaBrowser Client="Glance", Device="Glance", '
                        'DeviceId="glance-dashboard", Version="1.0", '
                        f'Token="{TOKEN}"'
                    )
                },
            )
            with open_http(request, timeout=30) as response:
                data = json.load(response)
            counts = {
                "MovieCount": int(data["MovieCount"]),
                "SeriesCount": int(data["SeriesCount"]),
                "EpisodeCount": int(data["EpisodeCount"]),
                "AlbumCount": int(data["AlbumCount"]),
                "SongCount": int(data["SongCount"]),
                "BoxSetCount": int(data["BoxSetCount"]),
            }
            with LOCK:
                COUNTS = counts
            delay = 300
        except Exception as error:
            logging.warning("Jellyfin counts refresh failed: %s", error)
            delay = 30
        time.sleep(delay)


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/counts":
            self.send_error(404)
            return

        with LOCK:
            counts = COUNTS
        status = 200 if counts is not None else 503
        body = json.dumps(counts if counts is not None else {"error": "warming up"}).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    threading.Thread(target=refresh_counts, daemon=True).start()
    ThreadingHTTPServer((BIND_ADDRESS, 8765), Handler).serve_forever()
