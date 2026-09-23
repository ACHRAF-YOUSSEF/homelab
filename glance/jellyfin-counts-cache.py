"""Serve the last successful Jellyfin library counts to Glance."""

import json
import logging
import os
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import Request, urlopen


JELLYFIN_URL = "http://host.docker.internal:8096/Items/Counts"
TOKEN = os.environ["JELLYFIN_API_KEY"]
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
            with urlopen(request, timeout=30) as response:
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
    ThreadingHTTPServer(("0.0.0.0", 8765), Handler).serve_forever()
