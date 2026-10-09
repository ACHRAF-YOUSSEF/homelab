"""HTTP-only requests for configured service endpoints, without redirects."""

from urllib.parse import urlsplit
from urllib.request import (
    HTTPDefaultErrorHandler,
    HTTPErrorProcessor,
    HTTPHandler,
    HTTPSHandler,
    OpenerDirector,
    ProxyHandler,
    Request,
    UnknownHandler,
)


def open_http(request, timeout):
    """Reject local-file schemes and redirects, including credential forwarding."""
    url = request.full_url if isinstance(request, Request) else request
    try:
        parts = urlsplit(url)
        port = parts.port
        if (parts.scheme not in ("http", "https") or not parts.hostname
                or parts.username is not None or parts.password is not None
                or parts.fragment or (port is not None and not 1 <= port <= 65535)
                or any(character.isspace() or ord(character) < 32 or ord(character) == 127
                       or character == "\\" for character in url)):
            raise ValueError
    except (TypeError, ValueError):
        raise ValueError("Service URL must use HTTP or HTTPS with a valid host and no embedded credentials") from None

    # Construct an explicit opener: no file/FTP/data or redirect handlers.
    # HTTPSHandler uses Python's normal certificate and hostname verification.
    opener = OpenerDirector()
    for handler in (ProxyHandler(), HTTPHandler(), HTTPSHandler(),
                    HTTPDefaultErrorHandler(), HTTPErrorProcessor(), UnknownHandler()):
        opener.add_handler(handler)
    return opener.open(request, timeout=timeout)
