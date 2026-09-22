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
docker compose up -d --force-recreate glance
```

The previous `homarr/` data directory is intentionally retained for rollback.
