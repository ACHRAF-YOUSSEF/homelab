"""Small HTTP client for explicitly configured local media services."""

from __future__ import annotations

import http.client
import json
import urllib.error
import urllib.parse
import urllib.request


class HTTPError(Exception):
    """A sanitized error that never includes request URLs or headers."""


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def validate_base_url(value: str) -> str:
    value = str(value or "").strip().rstrip("/")
    parsed = urllib.parse.urlsplit(value)
    if (parsed.scheme not in {"http", "https"} or not parsed.hostname or
            parsed.username or parsed.password or parsed.query or parsed.fragment or
            any(ord(char) < 32 or char == "\\" for char in value)):
        raise ValueError("Enter a valid HTTP or HTTPS server URL")
    try:
        parsed.port
    except ValueError as exc:
        raise ValueError("Enter a valid HTTP or HTTPS server URL") from exc
    return value


def request(base_url: str, path: str, *, method="GET", api_key="", body=None,
            timeout=8, max_bytes=2 * 1024 * 1024, api_key_header="X-Api-Key"):
    """Send one bounded request; redirects and non-HTTP URL handlers are disabled."""
    base_url = validate_base_url(base_url)
    if not path.startswith("/") or path.startswith("//") or "\\" in path:
        raise ValueError("Invalid API path")
    url = base_url + path
    headers = {"Accept": "application/json", "User-Agent": "subtitle-converter/1"}
    if api_key:
        if api_key_header not in {"X-Api-Key", "X-Emby-Token"}:
            raise ValueError("Unsupported API key header")
        headers[api_key_header] = api_key
    data = None
    if body is not None:
        data = json.dumps(body, separators=(",", ":")).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    opener = urllib.request.build_opener(_NoRedirect())
    try:
        with opener.open(req, timeout=timeout) as response:
            payload = response.read(max_bytes + 1)
            if len(payload) > max_bytes:
                raise HTTPError("Service response was too large")
            if not payload:
                return {}
            try:
                return json.loads(payload.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                raise HTTPError("Service returned an invalid response") from exc
    except HTTPError:
        raise
    except (urllib.error.URLError, TimeoutError, OSError, http.client.HTTPException) as exc:
        # Deliberately discard the exception text: it may contain a credential-bearing URL.
        raise HTTPError("Could not contact the configured service") from None
