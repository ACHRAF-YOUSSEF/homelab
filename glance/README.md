# Glance dashboard

The dashboard is defined in [config/glance.yml](config/glance.yml) and deployed by
[compose/infrastructure.yml](../compose/infrastructure.yml). Glance listens on
container port `8080`, published as host port `7575`. For a local installation,
open `http://localhost:7575` on Windows, macOS, or Linux. Set `GLANCE_PUBLIC_URL`
to the address your browser should use, and follow the
[platform guide](../README.md#platform-guide) when adapting host paths and
networking. DNS, tunnel routes, and reverse-proxy hosts are configured separately.

## Configuration layout

| Path | Purpose |
| --- | --- |
| [config/glance.yml](config/glance.yml) | Server settings, branding, theme, asset directory, script, and page includes |
| [config/pages/](config/pages/) | One YAML file per navigation tab |
| [config/widgets/](config/widgets/) | Reusable custom API widgets for Jellyfin, Sonarr/Radarr, Seerr, and qBittorrent |
| [config/assets/media-live.js](config/assets/media-live.js) | Refreshes selected Media widgets in the browser |
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
| Host and public service addresses | `TZ`, `HOMELAB_URL`, and `*_PUBLIC_URL` browser destinations |
| Private credentials | `JELLYFIN_API_KEY`, `SONARR_API_KEY`, `RADARR_API_KEY`, and `SEERR_API_KEY` for server-side API requests |
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
browser destinations and can use your own domains. Keep deployment addresses,
private links, and credentials in `.env`.

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
Recreate both Glance and the helper after changing this URL or the Jellyfin key.
Set `JELLYFIN_PUBLIC_URL` to the browser destination independently.

Sonarr, Radarr, Seerr, qBittorrent, and other container checks use the internal
base URL variables from the template, normally service names on `homelab`.
API keys are supplied to server-side requests; `media-live.js` fetches rendered
HTML from Glance.

The Overview Docker widget uses `GLANCE_DOCKER_HOST`, whose example value is
`tcp://docker-proxy:2375`. Glance joins the isolated `docker-monitoring` network
and does not mount the raw Docker socket. The proxy permits GET requests to
configured endpoint groups, rejects other HTTP methods with `POST=0`, and
publishes no host port. See
[Docker API access](../README.md#docker-api-access) for the other Docker clients.

## Security and proxy access

Glance uses the internal read-only Docker API proxy on the isolated `docker-monitoring` network. Its allowed API groups are configured in Compose; changes to those permissions change what connected monitoring clients can inspect. Glance's configuration and assets are mounted read-only. Keep API credentials and deployment-specific addresses in `.env`, and review the repository [security guidance](../SECURITY.md) before exposing services or changing proxy access.

## Custom widgets and refresh behavior

| Widget | Data source | Configured Glance cache |
| --- | --- | --- |
| [Jellyfin sessions](config/widgets/jellyfin-stats.yml) | `JELLYFIN_INTERNAL_URL` plus `/Sessions`; shows active playback, users, devices, play method, pause state, and progress | `15s` |
| [Jellyfin library](config/widgets/jellyfin-library.yml) | `GLANCE_JELLYFIN_COUNTS_INTERNAL_URL` plus `/counts`; movies, series, episodes, albums, songs, and collections | `15s` |
| [Upcoming episodes & movies](config/widgets/media-calendar.yml) | Sonarr and Radarr `/api/v3/calendar`; renders the next 14 days | `30s` |
| [Seerr requests](config/widgets/seerr-requests.yml) | Seerr `/api/v1/request/count`; totals, movies, shows, pending, processing, and available requests | `15s` |
| [qBittorrent](config/widgets/qbittorrent-stats.yml) | `/api/v2/transfer/info` and `/api/v2/torrents/info`; download speed, seeding/downloading counts, and a collapsible torrent list | `10s` |

The qBittorrent widget sends no credentials and performs no login. Its API calls
require qBittorrent to allow the Glance container through the Docker-network
authentication whitelist. An authentication failure displays an error directing
you to that whitelist. The widget defaults to `view: detailed` and
`mode: default`; `mode: upload` switches one summary statistic to upload speed.

On the Media page, `media-live.js` fetches
`/api/pages/media/content/` every 15 seconds after the previous refresh finishes.
It replaces changed content for the two service monitors, Jellyfin sessions,
Jellyfin library, the calendar, and Seerr requests. The two monitors have `30s`
server caches. Refreshing pauses while the browser tab is hidden and resumes
immediately when it becomes visible. Other pages and Media's release feed are
outside this script's refresh list. Cache durations determine when Glance can
fetch fresh source data; they do not guarantee that an open page polls at that
interval.

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
docker compose up -d glance
```

This starts the Docker proxy and library cache as dependencies. If you created
`compose.override.yaml` as described in the platform guide, use
`docker compose -f docker-compose.yml -f compose.override.yaml` in every command
below. The other
services used by dashboard widgets must also be started and configured; see the
[root setup guide](../README.md).

After editing YAML or assets, restart Glance to ensure the configuration is
loaded, then reload the browser to pick up script changes:

```shell
docker compose restart glance
```

After changing `.env`, recreate the containers so they receive the new values.
Recreate both services when changing `JELLYFIN_API_KEY` or
`JELLYFIN_INTERNAL_URL`:

```shell
docker compose up -d --force-recreate glance jellyfin-counts-cache
```

Inspect startup, API, or cache failures with:

```shell
docker compose ps glance jellyfin-counts-cache docker-proxy
docker compose logs --tail 100 glance jellyfin-counts-cache
```

If library counts stay unavailable, check the cache logs, `JELLYFIN_INTERNAL_URL`,
and `JELLYFIN_API_KEY`. Calendar and Seerr errors point to their API keys or
service availability. To add a page, create its YAML under `config/pages/` and
add an include to `config/glance.yml`; to live-refresh an additional Media
widget, give it a CSS class and add that class to `media-live.js`.
