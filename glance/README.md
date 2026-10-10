# Glance dashboard

The dashboard is defined in [config/glance.yml](config/glance.yml) and deployed by
[compose/infrastructure.yml](../compose/infrastructure.yml). Glance listens on
container port `8080` on the Compose network. The `glance-live` gateway proxies
the dashboard and publishes host port `7575` from its container port `8081`.
`GLANCE_BIND_ADDRESS` defaults to `127.0.0.1`, so the local example opens at
`http://localhost:7575` on Windows, macOS, or Linux. For LAN clients, set
`GLANCE_BIND_ADDRESS` in the ignored root `.env` to the Docker host's LAN
interface address and set `GLANCE_PUBLIC_URL` to a browser-reachable LAN URL.
The dashboard has no authentication and is intended for LAN access. Do not add
a public tunnel route, WAN port forward, or Internet-facing reverse-proxy host.
Follow the
[platform guide](../README.md#platform-guide) when adapting host paths and
networking. DNS, tunnel routes, and reverse-proxy hosts are configured separately.

## Configuration layout

| Path | Purpose |
| --- | --- |
| [config/glance.yml](config/glance.yml) | Server settings, branding, theme, asset directory, script, and page includes |
| [config/pages/](config/pages/) | One YAML file per navigation tab |
| [config/widgets/](config/widgets/) | Reusable custom API widgets for Jellyfin, Sonarr/Radarr, Seerr, and qBittorrent |
| [config/assets/media-live.js](config/assets/media-live.js) | Refreshes Media widgets outside the live Jellyfin session feed |
| [config/assets/widget-live.js](config/assets/widget-live.js) | Connects visible Media and Downloads widgets to the same-origin SSE stream |
| [live/server.py](live/server.py) | Internal Glance proxy and SSE relay for qBittorrent and Jellyfin |
| [live/test_server.py](live/test_server.py) | Offline relay tests using local fake upstream services |
| [config/assets/subtitle-studio.svg](config/assets/subtitle-studio.svg) | Local Subtitle Studio icon |
| [jellyfin-counts-cache.py](jellyfin-counts-cache.py) | Internal HTTP service caching Jellyfin library counts |

Compose mounts `glance/config` read-only at `/app/config`; assets are served from
`/assets/`. The dashboard uses a dark theme, wide pages, and mobile headers.
Branding, weather, service addresses, external bookmarks, and RSS source URLs
come from environment variables. Customize their example values in the ignored
root `.env` file; edit YAML when changing the dashboard layout or widget behavior.

## Pages

| Tab | Configured content |
| --- | --- |
| [Overview](config/pages/overview.yml) | SearxNG search with `!yt`, `!gh`, and `!r` shortcuts; clock; configured weather; calendar; Docker containers; core service status; quick links |
| [Media](config/pages/media.yml) | Jellyfin sessions and library totals; 14-day Sonarr/Radarr calendar; Seerr request counts; media and automation status including Subtitle Studio; links and project releases |
| [Downloads](config/pages/downloads.yml) | qBittorrent transfer and torrent statistics; download and processing service status; bookmarks; download stack releases |
| [Network](config/pages/network.yml) | AdGuard Home, Nginx Proxy Manager, Uptime Kuma, and Portracker status; public-service links and checks; infrastructure releases; administration links |
| [Tools](config/pages/tools.yml) | Tool status; AI, automation, document, and cloud links; latest Glance community widgets; developer links |
| [Gaming](config/pages/gaming.yml) | Twitch top games; store links; gaming Reddit feeds; YouTube videos; game deals |
| [News](config/pages/news.yml) | Security and technology RSS; Hacker News; Lobsters; Reddit feeds; tech YouTube videos; project releases |

Feed, video, weather, release, and community-widget content requires access to
the corresponding external services. Monitor widgets often check an internal
`check-url` while opening a public or local `url`; a successful internal check
does not establish that the public route works.

## Environment and addresses

Use the grouped entries in [.env.example](../.env.example) as the reference for
all required variable names. Run Compose commands from the repository root,
where [docker-compose.yml](../docker-compose.yml) includes the infrastructure
file and its shared environment file.

For a fresh checkout, create `.env` without replacing an existing file, then
fill in your own values. In PowerShell on Windows:

```powershell
if (-not (Test-Path -LiteralPath .env)) { Copy-Item .env.example .env }
```

In a terminal on macOS or Linux:

```bash
if [ ! -f .env ]; then cp .env.example .env; fi
```

| Template group | Configure |
| --- | --- |
| Host and browser service addresses | `TZ`, `HOMELAB_URL`, and `*_PUBLIC_URL` browser destinations, which may use local or reverse-proxy routes and do not publish services |
| LAN binding and live refresh | `GLANCE_BIND_ADDRESS` defaults to `127.0.0.1`; set the Docker host's LAN interface address privately for LAN clients. Compose adds that address to the gateway host allowlist. If browsers use a LAN DNS name, add it to comma-separated `GLANCE_LIVE_ALLOWED_HOSTS`. `GLANCE_LIVE_QBITTORRENT_INTERVAL` defaults to 2 seconds; `GLANCE_LIVE_JELLYFIN_INTERVAL` to 5 seconds; `GLANCE_LIVE_JELLYFIN_WS` enables Jellyfin WebSocket events with REST reconciliation/fallback. These polling intervals apply while a matching page is open and visible. |
| Private credentials | `JELLYFIN_API_KEY`, optional `QBITTORRENT_USERNAME` plus `QBITTORRENT_PASSWORD`, `SONARR_API_KEY`, `RADARR_API_KEY`, and `SEERR_API_KEY` for server-side requests |
| Docker API proxies | `GLANCE_DOCKER_HOST` for the internal read-only Docker proxy |
| Internal service base URLs | `JELLYFIN_INTERNAL_URL`, the media API URLs, and `GLANCE_*_INTERNAL_URL` values for checks and the counts helper |
| Glance appearance | `GLANCE_APP_NAME`, `GLANCE_LOGO_TEXT`, and `GLANCE_WEATHER_LOCATION` |
| Public external bookmarks and search providers | `GLANCE_*_URL` values for external links and search shortcuts |
| Glance public RSS feed sources | `GLANCE_*_FEED_URL` values for the news and AI feeds |

`HOMELAB_URL` is a scheme and hostname or IP without a port or trailing slash;
the pages append service ports for shared-host links, including the SearxNG
search destination on port `8088`. The template uses
`http://localhost` and `TZ=Etc/UTC` as examples. For another machine's browser,
use an address that machine can reach. Service-specific public URLs are complete
browser destinations and can point to a LAN address or reverse proxy. A URL
value does not publish the service or create DNS/tunnel routes. Keep deployment
addresses, private links, and credentials in `.env`.

Internal URLs are addresses reachable **from the containers** and have no
trailing slash. They are separate from public browser URLs. The Jellyfin example
uses `JELLYFIN_INTERNAL_URL=http://host.docker.internal:8096`; this repository does
not define a Jellyfin Compose service. Docker Desktop on Windows and macOS
provides `host.docker.internal` for access to the host. On native Linux Engine,
Glance and the library cache already declare
`extra_hosts: ["host.docker.internal:host-gateway"]`. Jellyfin must listen on an
address reachable from containers, and the host firewall must permit that traffic.

If Jellyfin runs in a separately managed container, attach it to the shared
`homelab` network and set `JELLYFIN_INTERNAL_URL` to its service name or network
alias, for example `http://jellyfin:8096`. The sessions widget appends `/Sessions`
and the counts helper appends `/Items/Counts`; no script constant needs editing.
Recreate `glance-live`, Glance, and the helper after changing this URL or the Jellyfin key.
Set `JELLYFIN_PUBLIC_URL` to the browser destination independently.

Sonarr, Radarr, Seerr, qBittorrent, and other container checks use the internal
base URL variables from the template, normally service names on `homelab`.
API keys and optional qBittorrent credentials are supplied only to server-side
requests. The live relay also proxies normal Glance page and asset requests, so
the browser uses the same origin for the dashboard and `/live/events`.

The Overview Docker widget uses `GLANCE_DOCKER_HOST`, whose example value is
`tcp://docker-proxy:2375`. Glance joins the isolated `docker-monitoring` network
and does not mount the raw Docker socket. The proxy permits GET requests to
configured endpoint groups, rejects other HTTP methods with `POST=0`, and
publishes no host port. See
[Docker API access](../README.md#docker-api-access) for the other Docker clients.

## Security and proxy access

The Glance dashboard and live stream have no user authentication. Keep host port `7575` on the LAN: `GLANCE_BIND_ADDRESS` defaults to loopback; for LAN devices set it privately to the Docker host's LAN interface address, restrict it with the host firewall, and do not configure WAN forwarding or a public tunnel route. If an existing reverse proxy is used only on the LAN, point its upstream at `glance-live:8081` (not `glance:8080`) and configure streaming responses without buffering; the Compose service itself publishes no other host port. A proxy or tunnel route is a separate exposure path and is not made private by the bind setting.

The gateway proxies Glance and emits sanitized JSON snapshots, keeping Jellyfin and optional qBittorrent credentials on the server. The `/live/events` route accepts only the `media` and `downloads` topics and applies same-origin checks; its request also probes the corresponding Glance page-content route with the browser cookie, preserving Glance's native authorization if enabled later. This probe does not add authentication to the current unauthenticated dashboard. The gateway runs as an unprivileged user, drops Linux capabilities, has a read-only filesystem, and is connected only to `homelab`; its image base and Python dependency are pinned. Glance remains on the internal `docker-monitoring` network for its read-only Docker API proxy and does not mount the raw Docker socket. Keep API credentials and deployment-specific addresses in `.env`, and review [SECURITY.md](../SECURITY.md) before changing exposure or network access.

## Custom widgets and refresh behavior

| Widget | Data source | Initial Glance cache / live refresh |
| --- | --- | --- |
| [Jellyfin sessions](config/widgets/jellyfin-stats.yml) | Live relay uses Jellyfin `/socket` `SessionsStart`, plus `/Sessions` snapshots/reconciliation; shows active playback, users, devices, play method, pause state, and progress | `15s` initial cache; then event-driven, with `5s` REST reconciliation by default |
| [Jellyfin library](config/widgets/jellyfin-library.yml) | `GLANCE_JELLYFIN_COUNTS_INTERNAL_URL` plus `/counts`; movies, series, episodes, albums, songs, and collections | `15s` |
| [Upcoming episodes & movies](config/widgets/media-calendar.yml) | Sonarr and Radarr `/api/v3/calendar`; renders the next 14 days | `30s` |
| [Seerr requests](config/widgets/seerr-requests.yml) | Seerr `/api/v1/request/count`; totals, movies, shows, pending, processing, and available requests | `15s` |
| [qBittorrent](config/widgets/qbittorrent-stats.yml) | Live relay `/api/v2/sync/maindata?rid=…`; transfer speed, seeding/downloading counts, and collapsible torrent list | `10s` initial cache; then incremental API polling every `2s` by default |

The qBittorrent relay can use optional `QBITTORRENT_USERNAME` and
`QBITTORRENT_PASSWORD` values kept in the ignored `.env`; configure both or
neither. If they remain blank, qBittorrent must allow the gateway container
through its network authentication whitelist. The first server-rendered widget
still uses Glance API subrequests without a login, so the Glance container must
also be allowed through qBittorrent's whitelist for that initial fallback. The
relay accepts qBittorrent's HTTP 204 empty login response and its legacy HTTP
200 `Ok.` response, retaining the returned session cookie for API requests and
reauthenticating after an authentication-required response. Recreate
`glance-live` after changing its credentials. The
widget defaults to `view: detailed` and `mode: default`; `mode: upload` switches
one summary statistic to upload speed. The fallback remains visible if browser
live updates are unavailable.

When Media or Downloads is open and visible, `widget-live.js` opens a same-origin
EventSource connection to `/live/events?page=media` or
`/live/events?page=downloads`. The gateway sends an initial snapshot, then
updates; EventSource reconnects after a disconnect. Only one collector per topic
runs while at least one browser subscribes, and it stops when the last client
leaves. The stream sends keepalives and closes after its configured maximum
connection lifetime so browsers reconnect periodically. Hidden tabs close the
live stream; returning to the page reconnects and receives a fresh snapshot.
qBittorrent's `/api/v2/sync/maindata` requests default
to every two seconds. Jellyfin uses WebSocket session events when enabled and
reconciles with `/Sessions` every five seconds; set
`GLANCE_LIVE_JELLYFIN_WS=false` to use REST polling instead. A paused Jellyfin
session does not animate progress; playing progress is interpolated for at most
15 seconds between confirmed snapshots and corrected on the next update.

The existing `media-live.js` continues to refresh the other selected Media
widgets (service monitors, library counts, calendar, and Seerr) from Glance's
rendered page content every 15 seconds while visible. The two service monitors
have `30s` server caches. Neither stream nor browser refresh updates Media's
release feed. Cache durations determine when Glance can fetch fresh source data;
they do not themselves push changes to an open page. If upstream calls fail,
the relay retains its last successful snapshot across reconnects and labels it
stale until a fresh upstream fetch succeeds. Inspect gateway logs for
provider/category errors, which do not print credentials or response bodies.

The library cache makes its first Jellyfin request at startup, uses a 30-second
request timeout, waits five minutes after success, and retries after 30 seconds
on failure. It retains the last successful counts in memory while retries fail.
Before the first success, `/counts` returns `503` with a warming-up response.
Restarting the cache clears those in-memory counts. The helper listens on
container port `8765` without publishing a host port.

## Maintenance

The Docker commands below work in PowerShell on Windows and terminals on macOS
and Linux. Validate Compose configuration and start Glance with its declared
dependencies:

```shell
docker compose config --quiet
docker compose up -d --build glance-live glance
```

This starts the Docker proxy and library cache as dependencies of Glance, plus
the gateway that publishes the dashboard on host port `7575`. If you created
`compose.override.yaml` as described in the platform guide, use
`docker compose -f docker-compose.yml -f compose.override.yaml` in every command
below. The other
services used by dashboard widgets must also be started and configured; see the
[root setup guide](../README.md).

After editing YAML or assets, restart Glance to ensure the configuration is
loaded, then reload the browser to pick up script changes:

```shell
docker compose restart glance glance-live
```

After changing `.env`, recreate the containers so they receive the new values.
Recreate all three services when changing `JELLYFIN_API_KEY` or
`JELLYFIN_INTERNAL_URL`:

```shell
docker compose up -d --build --force-recreate glance-live glance jellyfin-counts-cache
```

Inspect startup, API, or cache failures with:

```shell
docker compose ps glance glance-live jellyfin-counts-cache docker-proxy
docker compose logs --tail 100 glance-live glance jellyfin-counts-cache
```

After changing `GLANCE_BIND_ADDRESS`, recreate `glance-live` so Docker republishes
host port `7575` on the selected interface. Recreate `glance-live` after changing
its polling intervals, WebSocket setting, Jellyfin URL/key, or qBittorrent URL or
credentials. Changing the Jellyfin URL/key also requires recreating `glance` and
`jellyfin-counts-cache`; changing the qBittorrent URL also requires recreating
`glance` for its server-rendered fallback.

The gateway health route is `/live/health` at the same browser origin; it checks
the gateway process, not the Jellyfin or qBittorrent upstreams. For backend tests,
create an isolated Python environment in `glance/live`, install the pinned
`requirements.txt`, and run `python -m unittest test_server -v`. On Windows
PowerShell:

```powershell
Set-Location glance/live
py -3 -m venv .venv
.\.venv\Scripts\python.exe -m pip install -r requirements.txt
.\.venv\Scripts\python.exe -m unittest test_server -v
```

On macOS or Linux:

```bash
cd glance/live
python3 -m venv .venv
.venv/bin/python -m pip install -r requirements.txt
.venv/bin/python -m unittest test_server -v
```

The tests use disposable local HTTP/WebSocket fixtures and do not start Compose
services or connect to the real Jellyfin/qBittorrent deployments.
Check the browser script syntax from the repository root with
`node --check glance/config/assets/widget-live.js`.

If library counts stay unavailable, check the cache logs, `JELLYFIN_INTERNAL_URL`,
and `JELLYFIN_API_KEY`. Check qBittorrent credentials or its network whitelist
if the Downloads feed is stale. Calendar and Seerr errors point to their API
keys or service availability. To add a page, create its YAML under
`config/pages/` and add an include to `config/glance.yml`; to live-update another
widget, add its topic and renderer to `widget-live.js` and the gateway.
