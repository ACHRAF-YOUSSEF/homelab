# Glance dashboard

Glance replaces Homarr on host port `7575`, so the existing Nginx Proxy Manager
and Cloudflare route can continue pointing to the same host and port.

The main configuration is `config/glance.yml`. Each navigation tab is kept in a
separate file under `config/pages/`:

- Overview
- Media
- Downloads
- Network
- Tools
- Gaming
- News

Set `HOMELAB_URL` in the repository's root `.env` file to the scheme and LAN IP
used by browsers to open services which do not have a public hostname. The
default is `http://192.168.0.163`.

Glance reads Docker status through the existing read-only Docker socket proxy at
`tcp://docker-proxy:2375`; the Docker socket is not mounted into Glance.

Configuration files are watched and reloaded automatically. Changes to
environment variables require recreating the container:

```powershell
docker compose --env-file .env up -d glance --force-recreate
```

The Media page has a 14-day release calendar from Sonarr and Radarr, plus Seerr
request counts. Set `SONARR_API_KEY`, `RADARR_API_KEY`, and `SEERR_API_KEY` in the
root `.env` file. Get the Sonarr and Radarr keys from Settings > General > Security
and the Seerr key from Settings > General. Glance calls these services over the
internal Docker network; the keys are not placed in browser-side JavaScript.

While the Media page is open, its Jellyfin stats, Seerr requests, Sonarr/Radarr
calendar, and service status widgets refresh every 15 seconds. The browser fetches
rendered content from Glance rather than calling the services directly. Glance
caches Jellyfin for 5 seconds, Seerr for 15 seconds, and the calendar and status
widgets for 30 seconds. Refreshing pauses when the tab is hidden and resumes when
it becomes visible. The script is served from `config/assets/media-live.js`.

The previous `homarr/` data directory is intentionally retained for rollback.
