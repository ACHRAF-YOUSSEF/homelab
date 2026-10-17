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
| [config/widgets/](config/widgets/) | Reusable custom API widgets for Jellyfin, Sonarr/Radarr, Seerr, qBittorrent, and Subforge |
| [config/assets/media-calendar.js](config/assets/media-calendar.js) | Browser calendar controls, rendering, and same-origin calendar requests |
| [config/assets/media-calendar.css](config/assets/media-calendar.css) | Responsive calendar layout and disclosure styling |
| [config/assets/media-missing.js](config/assets/media-missing.js) | Same-origin missing-media inventory rendering, refresh, and disclosure state |
| [config/assets/media-missing.css](config/assets/media-missing.css) | Responsive TV/movie columns and show/season disclosure styling |
| [config/assets/subforge-live.js](config/assets/subforge-live.js) | Subforge live job-list rendering and bounded elapsed/ETA display |
| [config/assets/media-live.js](config/assets/media-live.js) | Refreshes Media widgets outside the live Jellyfin session feed |
| [config/assets/widget-live.js](config/assets/widget-live.js) | Connects visible Media and Downloads widgets to the same-origin SSE stream |
| [live/src/](live/src/) | Rust Glance proxy, bounded SSE relay, provider collectors, and configuration |
| [live/Cargo.toml](live/Cargo.toml) | Pinned gateway dependencies and release profile |
| [live/src/bin/glance-live-probe.rs](live/src/bin/glance-live-probe.rs) | Optional Rust load probe with fake loopback providers; excluded from the production image |
| [config/assets/subtitle-studio.svg](config/assets/subtitle-studio.svg) | Local Subtitle Studio icon |
| [config/assets/subforge.svg](config/assets/subforge.svg) | Current blue Subforge icon, used by the Overview Docker container entry and Media service monitor; the `v=2` asset query refreshes browser caches |
| [jellyfin-counts-cache/](jellyfin-counts-cache/) | Rust service caching Jellyfin library counts for Glance |

Compose mounts `glance/config` read-only at `/app/config`; assets are served from
`/assets/`. The dashboard uses a dark theme, wide pages, and mobile headers.
Branding, weather, service addresses, external bookmarks, and RSS source URLs
come from environment variables. Customize their example values in the ignored
root `.env` file; edit YAML when changing the dashboard layout or widget behavior.

## Pages

| Tab | Configured content |
| --- | --- |
| [Overview](config/pages/overview.yml) | SearxNG search with `!yt`, `!gh`, and `!r` shortcuts; clock; configured weather; calendar; Docker containers; core service status; quick links |
| [Media](config/pages/media.yml) | Jellyfin sessions and library totals; interactive Sonarr/Radarr calendar; missing Sonarr/Radarr media inventory; Seerr request counts; media and automation status including Subtitle Studio and Subforge; links and project releases |
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
| LAN binding and live refresh | `GLANCE_BIND_ADDRESS` defaults to `127.0.0.1`; set the Docker host's LAN interface address privately for LAN clients. Compose adds that address to the gateway host allowlist. If browsers use a LAN DNS name, add it to comma-separated `GLANCE_LIVE_ALLOWED_HOSTS`. `GLANCE_LIVE_QBITTORRENT_INTERVAL` defaults to 2 seconds; `GLANCE_LIVE_JELLYFIN_INTERVAL` to 5 seconds. `GLANCE_LIVE_JELLYFIN_WS` enables Jellyfin WebSocket events with REST reconciliation/fallback. These polling intervals apply while a matching page is open and visible. Subforge updates use native server-sent events and have no gateway polling interval. |
| Private credentials | `JELLYFIN_API_KEY`, optional `QBITTORRENT_USERNAME` plus `QBITTORRENT_PASSWORD`, `SONARR_API_KEY`, `RADARR_API_KEY`, and `SEERR_API_KEY` for server-side requests |
| Subforge widget | `CONVERTER_WIDGET_TOKEN`, a dedicated random bearer token used by Subforge and the `glance-live` gateway; blank disables the read-only widget stream |
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
and the Rust counts cache appends `/Items/Counts`; no source constant needs editing.
The cache stores only the six library count fields, refreshes every 300 seconds
after a successful fetch, and retries failures after 30 seconds while keeping
the previous successful counts. Its internal `/counts` endpoint is on container
port `8765`; it returns `503` until the first valid fetch. Its readiness health
check therefore remains unhealthy during warmup and becomes healthy once counts
are available. Recreate `glance-live`, Glance, and the cache after changing this
URL or the Jellyfin key.
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

The `subforge` container has an explicit **Subforge** display name, current
icon, and `${SUBFORGE_PUBLIC_URL}` link. Without that mapping, automatic name
formatting uses the service name and Docker's generic icon.

## Security and proxy access

The Glance dashboard and live stream have no user authentication. Keep host port `7575` on the LAN: `GLANCE_BIND_ADDRESS` defaults to loopback; for LAN devices set it privately to the Docker host's LAN interface address, restrict it with the host firewall, and do not configure WAN forwarding or a public tunnel route. If an existing reverse proxy is used only on the LAN, point its upstream at `glance-live:8081` (not `glance:8080`) and configure streaming responses without buffering; the Compose service itself publishes no other host port. A proxy or tunnel route is a separate exposure path and is not made private by the bind setting.

The gateway proxies the upstream Glance application and emits sanitized JSON snapshots, keeping Jellyfin and optional qBittorrent credentials on the server. The `/live/events` route accepts only the `media` and `downloads` topics and applies same-origin checks; its request also probes the corresponding Glance page-content route with the browser cookie, preserving Glance's native authorization if enabled later. This probe does not add authentication to the current unauthenticated dashboard.

`glance-live` uses one Tokio event-loop thread and at most two blocking DNS workers, with shared, on-demand collectors: a collector runs only while its topic has subscribers, and stops when the last browser disconnects. It has no GPU device access. For live streams and the calendar, provider responses are limited to 2 MiB, normalized snapshots to 256 KiB, torrent lists to 2,000 entries, Jellyfin sessions to 256, active SSE clients to 64, accepted HTTP connections to 128, and SSE lifetime to 900 seconds by default. Missing-media inventory uses its own larger provider and snapshot bounds described below. If an upstream payload exceeds its bound or fails, the topic is marked stale and its last valid snapshot is retained; the gateway does not synthesize replacement values. These defaults are configurable through `GLANCE_LIVE_*` variables described below.

The gateway runs as an unprivileged user, drops Linux capabilities, has a read-only filesystem, and is connected only to `homelab`; its Rust toolchain and runtime image are pinned. Glance itself remains upstream and continues on the internal `docker-monitoring` network for its read-only Docker API proxy; it does not mount the raw Docker socket. Keep API credentials and deployment-specific addresses in `.env`, and review [SECURITY.md](../SECURITY.md) before changing exposure or network access.

The Compose gateway has a configurable 96 MiB memory ceiling, a 0.50 CPU ceiling, and a 32-process limit. The memory value is an upper bound, not an expected working set or a reservation. See the [isolated migration measurements](../docs/memory-budget.md#glance-live-migration-measurements) for observed usage and test limits.

## Custom widgets and refresh behavior

| Widget | Data source | Initial Glance cache / live refresh |
| --- | --- | --- |
| [Jellyfin sessions](config/widgets/jellyfin-stats.yml) | Live relay uses Jellyfin `/socket` `SessionsStart`, plus `/Sessions` snapshots/reconciliation; shows active playback, users, devices, play method, pause state, and progress | `15s` initial cache; then event-driven, with `5s` REST reconciliation by default |
| [Jellyfin library](config/widgets/jellyfin-library.yml) | `GLANCE_JELLYFIN_COUNTS_INTERNAL_URL` plus `/counts`; movies, series, episodes, albums, songs, and collections | `15s` |
| [Upcoming episodes & movies](config/widgets/media-calendar.yml) | Same-origin `/live/calendar` gateway route backed by Sonarr and Radarr `/api/v3/calendar`; renders day, Monday-first week, month, or year views | Browser refresh every `30s` while visible |
| [Missing from disk](config/widgets/media-missing.yml) | Same-origin `/live/missing`; monitored Sonarr episodes and Radarr movies without files | Browser refresh every `30s` while visible; last successful inventory remains visible on refresh failure |
| [Seerr requests](config/widgets/seerr-requests.yml) | Seerr `/api/v1/request/count`; totals, movies, shows, pending, processing, and available requests | `15s` |
| [Subforge](config/widgets/subforge.yml) | JavaScript-rendered, same-origin SSE topic backed by Subforge's authenticated `/api/widget/events`; processed-file totals and up to 32 concurrent job summaries with progress, elapsed time, approximate ETA, liveness, and generic wait status | Native SSE while the Media page is visible; no browser REST fallback |
| [qBittorrent](config/widgets/qbittorrent-stats.yml) | Live relay `/api/v2/sync/maindata?rid=…`; transfer speed, seeding/downloading counts, and collapsible torrent list | `10s` initial cache; then incremental API polling every `2s` by default |

The Subforge widget is optional. The `subforge` service starts with
the regular Compose stack after its selected local image has been built. Set a
private random `CONVERTER_WIDGET_TOKEN` of 32–256 characters in the ignored root
`.env`; Compose supplies it only to the `glance-live` gateway and Subforge. It
is separate from dashboard, webhook, and media-server credentials. The widget
stream returns only aggregate status and does not expose media paths or
credentials, and it does not accept actions. It includes
`active_jobs` (up to 32, oldest first) and `active_job_count`, while retaining
`current_run` for compatibility with older clients. Glance uses the active job
list to show concurrent work and displays safe job IDs, phase/status, file
progress when available, elapsed time, nullable ETA, liveness, and a generic
wait reason. The HTML shell requires JavaScript and EventSource; it has no REST
fallback or initial server-rendered widget data. The estimate
uses completed-file throughput and has the same limits as Subforge's dashboard:
a single-file job has no ETA until the file completes, and quiet OCR may look
possibly stalled while it is working. These states are diagnostic; they do not
fail jobs automatically. Elapsed time and known ETA are interpolated for at
most 15 seconds after a snapshot;
the displayed ETA remains approximate.
Its link uses `SUBFORGE_PUBLIC_URL` (default
`http://localhost:8100`); its server-side request uses
`GLANCE_SUBFORGE_INTERNAL_URL` (default
`http://subforge:8100`). When upgrading from the former service name, change
an explicit `.env` value that still points to `subtitle-converter` to
`http://subforge:8100`. The internal URL must be reachable on the
Compose network. A blank token disables the endpoint; a 401 response means the
two token values do not match.

The separate Subforge service monitor checks the service's unauthenticated
`/health` endpoint. This reports whether the service is reachable even when the
optional widget token is blank. The widget token remains a separate credential
from Subforge's dashboard Basic Authentication, which is enabled by default
and can be disabled with `CONVERTER_AUTH_ENABLED=false` in the ignored root
`.env`. Disabling dashboard login does not enable the widget endpoint or remove
its bearer-token check.

To deploy code changes, rebuild the gateway and the selected local Subforge
image, then recreate the services. After changing only the token in `.env`,
recreate `glance-live`, Glance, and Subforge so they receive the same value;
an image rebuild is unnecessary for environment-only changes. Compose builds
`glance-live`; build the versioned Subforge image with its Dockerfile. The
default CPU selection uses:

```shell
docker build --file subforge/Dockerfile --tag homelab-subforge:0.4.4-cpu subforge
docker compose build glance-live
docker compose up -d --force-recreate glance-live glance subforge
```

Run the matching command from the repository root. For a CUDA deployment, use
the CUDA override so recreating the service keeps its selected image:

```shell
docker build --platform linux/amd64 --file subforge/Dockerfile.cuda --tag homelab-subforge:0.4.4-cuda subforge
docker compose -f docker-compose.yml -f compose/subforge.cuda.yml build glance-live
docker compose -f docker-compose.yml -f compose/subforge.cuda.yml up -d --force-recreate glance-live glance subforge
```

Run these commands from the repository root in PowerShell, Bash, or Zsh. Build
the images before applying them, and wait for active conversion jobs
to finish before recreating Subforge; shutdown cancels in-memory jobs. These
commands preserve the named `subtitle_converter_data` volume. Do not use
`down -v`. After changing the Subforge internal URL, recreate Glance and
`glance-live` with the same Compose files for the selected edition. A public-URL
change only requires recreating Glance. After
changing Glance YAML or assets, restart Glance and reload the browser. Browser
checks from the repository root: `node --test glance/test_subforge_live.cjs`;
gateway fixture checks: `cargo test --locked --manifest-path glance/live/Cargo.toml`.

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

When Media or Downloads is open and visible, `widget-live.js` opens same-origin
EventSource connections to `/live/events?page=media` (Jellyfin) or
`/live/events?page=downloads` (qBittorrent). The Subforge widget opens a second
Media-page connection at `/live/events?page=media&topic=subforge`; thus a Media
page with both widgets uses two streams. The gateway sends an initial snapshot,
then updates; EventSource reconnects after a disconnect. Only one collector per
topic runs while at least one browser subscribes, and it stops when the last
client leaves. For Subforge, the gateway opens one authenticated upstream
`/api/widget/events` SSE connection per subscribed topic; Subforge sends an
initial snapshot, coalesced updates after changes, and 15-second heartbeats.
The stream closes after its 15-minute maximum lifetime so browsers reconnect
periodically. The gateway bounds upstream records and normalized snapshots,
does not follow redirects, and backs off from 500 ms to 30 seconds on errors.
Hidden tabs close the live stream; returning to the page reconnects and receives
a fresh snapshot.
Upstream failures retain the last successful Subforge snapshot and mark it
stale. Browser elapsed-time and known-ETA interpolation stops after 15 seconds;
this is a display interpolation bound, not the provider stale threshold.
Progress and ETA values otherwise change only when a new snapshot reports them;
the browser does not invent progress or an ETA when the source has none.
qBittorrent's `/api/v2/sync/maindata` requests default
to every two seconds. Jellyfin uses WebSocket session events when enabled and
reconciles with `/Sessions` every five seconds; set
`GLANCE_LIVE_JELLYFIN_WS=false` to use REST polling instead. A paused Jellyfin
session does not animate progress; playing progress is interpolated for at most
15 seconds between confirmed snapshots and corrected on the next update.

The existing `media-live.js` continues to refresh the other selected Media
widgets (service monitors, library counts, and Seerr) from Glance's rendered
page content every 15 seconds while visible. The calendar has its own browser
refresh every 30 seconds while visible. It starts on the current Monday-first
week, offers day, week, month, and year views, and has previous/next plus
Today, This week, This month, and This year controls. The year view shows all
12 months and scheduled release days. More than three episodes from the same
series on one browser-local day appear in a collapsed native disclosure; up to
three remain listed individually. Navigation resets disclosures, while refresh
preserves navigation and expansion state. Episode timestamps use the browser's
local timezone; movie release dates remain date-only calendar days. The HTML
shell includes Glance's standard widget header and frame to preserve the normal
spacing from Subforge above and Automation below.

The two service monitors
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

### Sonarr and Radarr calendar troubleshooting

The browser fetches `/live/calendar?start=YYYY-MM-DD&end=YYYY-MM-DD` from the
same origin as Glance; `end` is exclusive. The gateway calls the configured
Sonarr and Radarr calendar APIs server-side, so API keys are never sent to the
browser. Compose supplies the existing `SONARR_INTERNAL_URL`, `SONARR_API_KEY`,
`RADARR_INTERNAL_URL`, and `RADARR_API_KEY` values to `glance-live`. Leave a
provider key blank to skip that provider; both keys blank return HTTP `503`.
Invalid ranges return `400`, same-origin/auth failures are rejected, and
configured provider or response-limit failures return `502`. The gateway
accepts ranges up to 366 days, pads provider requests by one day at each edge
for local-time conversion, allows at most two concurrent calendar requests,
and enforces provider and output size limits. Its HTTP client does not follow
redirects. The UI requires JavaScript.

If either provider returns HTTP `400`, check its **Settings > General > Host > Allowed
Hosts** list. Preserve existing entries and add `sonarr` for Sonarr or `radarr`
for Radarr, matching the hostname in the corresponding `*_INTERNAL_URL`. Enter
only the hostname: no scheme or port. Save the setting and restart the affected
service; the Arr host setting takes effect after restart. Do not clear the
allowlist or disable authentication.

HTTP `502` points to provider authentication, reachability, invalid provider
data, or a configured response limit; verify the matching API key in private
`.env` and check service status and internal URLs. After changing a provider
URL or API key in `.env`, rebuild and recreate `glance-live` so it receives the
new values. After editing the Glance YAML or assets, restart Glance and reload
the browser. These commands work in PowerShell, Bash, and Zsh from the
repository root:

```shell
docker compose up -d --build --force-recreate glance-live
docker compose restart glance
```

The calendar browser tests use local fixtures and do not contact Sonarr or
Radarr. Run them from the repository root with `node --test
glance/test_media_calendar.cjs` (PowerShell, Bash, or Zsh).

### Missing from disk widget

The Media page places **Missing from disk** immediately below the calendar.
It requests `GET /live/missing` from the Glance same origin. The gateway checks
the browser's media-page authorization through Glance, forwarding its session
cookie for that check, then calls Arr APIs server-side; provider API keys never
reach the browser. Configure the existing `SONARR_INTERNAL_URL`,
`SONARR_API_KEY`, `RADARR_INTERNAL_URL`, and `RADARR_API_KEY` variables. A blank
key marks that provider as not configured in the widget. If both keys are
blank, the endpoint returns `503`. The endpoint accepts same-origin requests
only and does not accept query parameters.

Sonarr inventory uses paginated `/api/v3/wanted/missing` requests with
`includeSeries=true` and `monitored=true`. The gateway also filters out entries
that Sonarr explicitly marks as unmonitored at the episode, series, or season
level. Monitored specials remain included. Sonarr's endpoint includes episodes
after airing and excludes future or undated episodes. Radarr inventory uses
`/api/v3/movie` and keeps only movies explicitly marked monitored that have no
movie file; unmonitored or unknown-monitoring movies are excluded, while
monitored unreleased movies remain included. “Missing” reflects the latest scan
state reported by Sonarr or Radarr; the gateway does not probe the filesystem
independently.

The widget shows TV and movies in responsive columns. A show with multiple
missing episodes starts collapsed by show and then by season; season `0` is
labelled **Specials**. A show with one missing episode is a flat entry. Refresh
preserves open disclosures and restores keyboard focus when the focused entry
still exists. The browser refreshes every 30 seconds while the page is visible,
keeps the last successful inventory after a refresh error, and exposes a
**Retry** button. Returning to a visible tab triggers a fresh request.

Missing inventories can be larger than calendar and live-stream payloads, so
they have separate configurable size bounds. The Sonarr and Radarr inputs and
the final inventory are each limited to 10,000 records. By default,
`GLANCE_LIVE_MISSING_MAX_PROVIDER_BYTES` allows 8 MiB per provider response
(maximum 16 MiB), and `GLANCE_LIVE_MISSING_MAX_SNAPSHOT_BYTES` allows a 2 MiB
normalized response (maximum 4 MiB). These settings do not change the general
gateway bounds: `GLANCE_LIVE_MAX_PROVIDER_BYTES` remains 2 MiB by default and
`GLANCE_LIVE_MAX_SNAPSHOT_BYTES` remains 256 KiB for the calendar and other
live payloads. A load has a 45-second overall timeout. Missing-inventory
requests share two slots with calendar requests; when both are occupied, the
endpoint returns `503`. An upstream, data, or size failure returns `502` and
does not replace the previous browser-rendered inventory.

After changing gateway code or Sonarr/Radarr keys, rebuild and recreate
`glance-live`; restart Glance to load updated page YAML and browser assets:

```shell
docker compose up -d --build --force-recreate glance-live
docker compose restart glance
```

Run the missing-media browser regression checks from the repository root on
Windows PowerShell, Bash, or Zsh:

```shell
node --test glance/test_media_missing.cjs
```

These use local fixtures and do not contact Sonarr, Radarr, or deploy the
Compose stack.

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

After changing `.env`, recreate the containers so they receive the new values. The gateway resource variables are `GLANCE_LIVE_MEMORY_LIMIT`, `GLANCE_LIVE_CPUS`, `GLANCE_LIVE_MAX_CONNECTIONS`, `GLANCE_LIVE_MAX_HTTP_CONNECTIONS`, `GLANCE_LIVE_SSE_MAX_SECONDS`, `GLANCE_LIVE_MAX_PROVIDER_BYTES`, `GLANCE_LIVE_MAX_SNAPSHOT_BYTES`, `GLANCE_LIVE_MISSING_MAX_PROVIDER_BYTES`, `GLANCE_LIVE_MISSING_MAX_SNAPSHOT_BYTES`, `GLANCE_LIVE_MAX_TORRENTS`, and `GLANCE_LIVE_MAX_SESSIONS`. Compose enforces the memory and CPU ceilings. The application validates connection, lifetime, payload, torrent, and session limits on startup and caps their accepted values. Recreate `glance-live` after changing any of them.
After changing cache code, rebuild and recreate only that service:

```shell
docker compose build jellyfin-counts-cache
docker compose up -d --no-deps jellyfin-counts-cache
```

When changing `JELLYFIN_API_KEY` or `JELLYFIN_INTERNAL_URL`, recreate the
cache, Glance, and gateway so they receive the updated values:

```shell
docker compose up -d --force-recreate glance-live glance jellyfin-counts-cache
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
the gateway process, not the Jellyfin or qBittorrent upstreams. The container's
health check uses the binary's `--healthcheck` mode, which makes a local request
to that route. For local development, install the Rust 1.90 toolchain and run
the locked checks from `glance/live`. On Windows PowerShell:

```powershell
Set-Location glance/live
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo clippy --all-targets --locked --features fixture-probe -- -D warnings
cargo test --all-targets --locked --features fixture-probe
cargo run --locked
```

On macOS or Linux:

```bash
cd glance/live
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo clippy --all-targets --locked --features fixture-probe -- -D warnings
cargo test --all-targets --locked --features fixture-probe
cargo run --locked
```

The gateway tests use local fixtures and do not start Compose services or
connect to the real Jellyfin/qBittorrent deployments. `cargo run --locked`
starts the gateway using its environment configuration; it expects Glance and
provider URLs to be reachable for full operation. The gateway, its fixtures,
and its optional load probe are Rust and require no Python. The probe is enabled
only by the `fixture-probe` Cargo feature and has a separate Docker `probe`
target; the default image contains only the gateway binary. CI runs it in the
gateway's disposable, isolated network namespace. No GPU is required or used.
Check the browser script syntax from the repository root with
`node --check glance/config/assets/widget-live.js`.

The counts cache is a native Rust service with no Python runtime or
cache-specific Python source. Its current-thread Tokio runtime allows at most
two blocking DNS workers. The server accepts at most eight concurrent clients,
limits each request to 8 KiB and 32 headers, and closes connections within five
seconds. Jellyfin JSON is limited to 16 KiB and a 30-second timeout. HTTPS uses
bundled public WebPKI roots and rejects redirects. These
bounds and Compose ceilings limit configured resources; they are not measured
usage or guaranteed host savings. For local development, install Rust 1.90 and
run from `glance/jellyfin-counts-cache`. PowerShell:

```powershell
Set-Location glance/jellyfin-counts-cache
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo audit --file Cargo.lock --deny warnings
cargo run --locked
```

macOS or Linux:

```bash
cd glance/jellyfin-counts-cache
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo audit --file Cargo.lock --deny warnings
cargo run --locked
```

`cargo run --locked` expects valid `JELLYFIN_INTERNAL_URL` and
`JELLYFIN_API_KEY` values and a reachable Jellyfin service. Tests use local
fixtures. Inspect actual container usage with
`docker stats --no-stream jellyfin-counts-cache`; results vary by host and
workload. The cache does not use a GPU.

If library counts stay unavailable, check the cache logs, `JELLYFIN_INTERNAL_URL`,
and `JELLYFIN_API_KEY`. Check qBittorrent credentials or its network whitelist
if the Downloads feed is stale. For calendar failures, see [Sonarr and Radarr
calendar troubleshooting](#sonarr-and-radarr-calendar-troubleshooting); Seerr
errors point to its API key or service availability. To add a page, create its YAML under
`config/pages/` and add an include to `config/glance.yml`; to live-update another
widget, add its topic and renderer to `widget-live.js` and the gateway.
