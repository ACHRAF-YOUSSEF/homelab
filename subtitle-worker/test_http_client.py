import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import Request

from http_client import open_http


class HTTPClientSecurityTests(unittest.TestCase):
    def setUp(self):
        self.requests = []
        self.destination_requests = []
        destination_requests = self.destination_requests

        class Destination(BaseHTTPRequestHandler):
            def do_GET(self):
                destination_requests.append(dict(self.headers))
                self.send_response(200)
                self.end_headers()

            def log_message(self, *args):
                pass

        self.destination = self.start_server(Destination)
        destination_url = f"http://127.0.0.1:{self.destination.server_port}/secret"
        requests = self.requests

        class Endpoint(BaseHTTPRequestHandler):
            def do_GET(self):
                requests.append((self.path, dict(self.headers), None))
                if self.path.startswith("/redirect/"):
                    location = {
                        "other": destination_url,
                        "file": "file:///synthetic-local-file",
                        "data": "data:text/plain,synthetic",
                        "same": "/json",
                    }[self.path.removeprefix("/redirect/")]
                    self.send_response(302)
                    self.send_header("Location", location)
                    self.end_headers()
                    return
                if self.path == "/missing":
                    self.send_error(404)
                    return
                self.reply({"result": "ok"})

            def do_POST(self):
                payload = self.rfile.read(int(self.headers["Content-Length"]))
                requests.append((self.path, dict(self.headers), payload))
                self.reply(json.loads(payload))

            def reply(self, data):
                payload = json.dumps(data).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args):
                pass

        self.server = self.start_server(Endpoint)
        self.base = f"http://127.0.0.1:{self.server.server_port}"

    def start_server(self, handler):
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(thread.join, 2)
        self.addCleanup(server.shutdown)
        return server

    def test_get_and_json_post_preserve_headers_and_payload(self):
        with open_http(self.base + "/json", timeout=2) as response:
            self.assertEqual(json.load(response), {"result": "ok"})
        request = Request(self.base + "/json", data=b'{"cue":"example"}',
                          headers={"Authorization": "fixture", "Content-Type": "application/json"})
        with open_http(request, timeout=2) as response:
            self.assertEqual(json.load(response), {"cue": "example"})
        self.assertEqual(self.requests[-1][1]["Authorization"], "fixture")

    def test_non_http_schemes_are_rejected_before_network_or_file_access(self):
        for url in ("file:///synthetic-local-file", "data:text/plain,synthetic",
                    "ftp://localhost/file", "gopher://localhost/file"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                open_http(url, timeout=2)
        self.assertEqual(self.requests, [])

    def test_invalid_hosts_credentials_ports_and_control_characters_are_rejected(self):
        for url in ("http:///missing-host", "http://user:password@localhost/",
                    "http://localhost:0/", "http://localhost:65536/",
                    "http://localhost:invalid/", self.base + "/#fragment",
                    self.base + "/\n", self.base + "/\x00", self.base + "/\x7f",
                    self.base + "/\\file"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                open_http(url, timeout=2)
        self.assertEqual(self.requests, [])

    def test_redirects_cannot_access_files_or_forward_credentials(self):
        for kind in ("other", "file", "data", "same"):
            request = Request(self.base + "/redirect/" + kind,
                              headers={"Authorization": "fixture"})
            with self.subTest(kind=kind), self.assertRaises(HTTPError) as error:
                open_http(request, timeout=2)
            self.assertEqual(error.exception.code, 302)
            error.exception.close()
        self.assertEqual(self.destination_requests, [])
        self.assertEqual(len(self.requests), 4)

    def test_http_failures_remain_http_errors_for_existing_retry_logic(self):
        with self.assertRaises(HTTPError) as error:
            open_http(self.base + "/missing", timeout=2)
        self.assertEqual(error.exception.code, 404)
        error.exception.close()
