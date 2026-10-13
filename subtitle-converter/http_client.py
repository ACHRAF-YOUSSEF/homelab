"""Small HTTP client for explicitly configured local media services."""

from __future__ import annotations

import http.client
import json
import socket
import urllib.error
import urllib.parse
import urllib.request


class HTTPError(Exception):
    """A categorized, sanitized error that never includes URLs or headers."""

    def __init__(self, message, *, category="service_error", status_code=None):
        super().__init__(message)
        self.category = category
        self.status_code = status_code


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


def request(base_url: str, path: str, *, method="GET", api_key="", body=None, form=None,
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
        if form is not None:
            raise ValueError("Choose either JSON or form request data")
        data = json.dumps(body, separators=(",", ":")).encode("utf-8")
        headers["Content-Type"] = "application/json"
    elif form is not None:
        if not isinstance(form, dict):
            raise ValueError("Form request data must be an object")
        data = urllib.parse.urlencode(form).encode("ascii")
        headers["Content-Type"] = "application/x-www-form-urlencoded"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    opener = urllib.request.build_opener(_NoRedirect())
    try:
        with opener.open(req, timeout=timeout) as response:
            payload = response.read(max_bytes + 1)
            if len(payload) > max_bytes:
                raise HTTPError("Service response was too large", category="response_limit")
            if not payload:
                return {}
            try:
                return json.loads(payload.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                raise HTTPError("Service returned an invalid response", category="invalid_json") from None
    except HTTPError:
        raise
    except urllib.error.HTTPError as exc:
        code = int(exc.code)
        # HTTPError owns the error response stream/socket. We intentionally do
        # not inspect the body (it may contain sensitive service diagnostics),
        # but still close it before replacing the exception with a safe one.
        try:
            exc.close()
        except OSError:
            pass
        category = "authentication" if code in {401, 403} else "http_status"
        message = "Service rejected the API key" if code in {401, 403} else "Service returned an HTTP error"
        raise HTTPError(message, category=category, status_code=code) from None
    except (TimeoutError, socket.timeout) as exc:
        raise HTTPError("Service request timed out", category="timeout") from None
    except urllib.error.URLError as exc:
        reason = exc.reason
        if isinstance(reason, (TimeoutError, socket.timeout)):
            raise HTTPError("Service request timed out", category="timeout") from None
        if isinstance(reason, ConnectionRefusedError):
            raise HTTPError("Service refused the connection", category="refused") from None
        raise HTTPError("Could not contact the configured service", category="unreachable") from None
    except (OSError, http.client.HTTPException) as exc:
        if isinstance(exc, ConnectionRefusedError):
            raise HTTPError("Service refused the connection", category="refused") from None
        if isinstance(exc, (TimeoutError, socket.timeout)):
            raise HTTPError("Service request timed out", category="timeout") from None
        # Deliberately discard the exception text: it may contain a credential-bearing URL.
        raise HTTPError("Could not contact the configured service", category="unreachable") from None
