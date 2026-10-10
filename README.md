<div align="center">

# Homelab

**A self-hosted infrastructure stack built on Docker Compose**

[![Cloudflared](https://img.shields.io/badge/Cloudflared-Cloudflare%20Tunnel-F38020?style=for-the-badge&logo=cloudflare&logoColor=white)](https://github.com/cloudflare/cloudflared)
[![AdGuard Home](https://img.shields.io/badge/AdGuard%20Home-DNS%20Filtering-68BC71?style=for-the-badge&logo=adguard&logoColor=white)](https://github.com/AdguardTeam/AdGuardHome)
[![Nginx Proxy Manager](https://img.shields.io/badge/Nginx%20Proxy%20Manager-Reverse%20Proxy-009639?style=for-the-badge&logo=nginx&logoColor=white)](https://github.com/NginxProxyManager/nginx-proxy-manager)
[![Nextcloud AIO](https://img.shields.io/badge/Nextcloud-AIO-0082C9?style=for-the-badge&logo=nextcloud&logoColor=white)](https://github.com/nextcloud/all-in-one)
[![Jellyfin](https://img.shields.io/badge/Jellyfin-Media%20Server-00A4DC?style=for-the-badge&logo=jellyfin&logoColor=white)](https://jellyfin.org/)

> A self-hosted homelab example for Windows, macOS, and Linux, with 32 Compose service definitions covering media automation, local AI, subtitles, DNS, cloud storage, remote access, and monitoring. Adapt the example paths, networking, and hardware settings for your system using the platform guide below.

</div>

---

## Table of Contents

- [Homelab](#homelab)
  - [Table of Contents](#table-of-contents)
  - [Overview](#overview)
  - [Architecture](#architecture)
  - [Services](#services)
  - [Compose Layout](#compose-layout)
  - [Component Guides](#component-guides)
  - [Prerequisites](#prerequisites)
  - [Platform Guide](#platform-guide)
    - [Choose Your Runtime](#choose-your-runtime)
    - [Portable Paths and Local Overrides](#portable-paths-and-local-overrides)
    - [Machines Without NVIDIA GPUs](#machines-without-nvidia-gpus)
    - [Host Networking and Permissions](#host-networking-and-permissions)
  - [Setup Guide](#setup-guide)
    - [1. Clone the Repository](#1-clone-the-repository)
    - [2. Configure Environment Variables](#2-configure-environment-variables)
    - [3. Configure DNS (AdGuard Home + Unbound)](#3-configure-dns-adguard-home--unbound)
    - [4. Start the Stack](#4-start-the-stack)
    - [5. Configure Nginx Proxy Manager](#5-configure-nginx-proxy-manager)
    - [6. Configure Jellyfin Direct Access](#6-configure-jellyfin-direct-access)
    - [7. Configure Nextcloud AIO](#7-configure-nextcloud-aio)
    - [8. Set Up Cloudflare Tunnel](#8-set-up-cloudflare-tunnel)
  - [Environment Variables Reference](#environment-variables-reference)
  - [Docker API Access](#docker-api-access)
  - [Port Reference](#port-reference)
  - [Media Stack Setup](#media-stack-setup)
    - [Jellyfin Plugins](#jellyfin-plugins)
  - [AI Stack Setup](#ai-stack-setup)
    - [Open WebUI (Local LLM Interface)](#open-webui-local-llm-interface)
    - [SearxNG (Private Search Engine)](#searxng-private-search-engine)
  - [DNS Blocklists](#dns-blocklists)
    - [Recommended Lists for AdGuard Home](#recommended-lists-for-adguard-home)
  - [Dashboard and Public URLs](#dashboard-and-public-urls)
  - [Security Audits](#security-audits)
  - [Development and Validation](#development-and-validation)
  - [Screenshots](#screenshots)
  - [Projects Used](#projects-used)
  - [Star History](#star-history)

---

## Overview

This repository is an example to adapt to your own homelab. Service addresses, public URLs, dashboard branding, and integration credentials are supplied through the root `.env` file. The base media paths and GPU reservations still need to match your host; the [Platform Guide](#platform-guide) covers Windows, macOS, and Linux, including machines without NVIDIA and native CPU subtitles.

Replace the example domain, LAN address, timezone, credentials, and media folders before using the stack. Choose the services you need; starting every definition is appropriate only after configuring its dependencies and hardware. Jellyfin and LM Studio are separate host applications in the example, and can be installed for your operating system or replaced with other reachable deployments.

- **Public access** - Cloudflared uses `CLOUDFLARE_TUNNEL_TOKEN` for tunnel routes. Cloudflare DDNS uses the domain list in `CLOUDFLARE_DDNS_DOMAINS` and sets `PROXIED=false` for those records. Supply your own domain list; tunnel routes, DNS records, and certificates are configured separately.
- **Direct HTTPS access** - Nginx Proxy Manager publishes HTTP on host port `80` and HTTPS on host port `4443`. A router serving standard HTTPS must forward WAN TCP `443` to host TCP `4443`.
- **Network-wide ad/tracker blocking** - [AdGuard Home](https://adguard.com/en/adguard-home/overview.html) backed by a recursive [Unbound](https://unbound.docs.nlnetlabs.nl/) DNS resolver and DNS blocklists.
- **Automated media pipeline** - Sonarr, Radarr, Bazarr, Prowlarr, qBittorrent, Tdarr, and Seerr for media requests.
- **YouTube archiving** - [TubeArchivist](https://github.com/tubearchivist/tubearchivist) for self-hosted YouTube download, indexing, and management, backed by Elasticsearch and Redis.
- **Local AI** - [Open WebUI](https://github.com/open-webui/open-webui) with NVIDIA GPU acceleration, connected to [LM Studio](https://lmstudio.ai/) for local LLM inference.
- **Self-hosted cloud** - [Nextcloud All-in-One](https://github.com/nextcloud/all-in-one) manages the Nextcloud deployment.
- **Automation** - [n8n](https://github.com/n8n-io/n8n) for workflow automation.
- **Local subtitles** - Subtitle Studio transcribes media with Faster Whisper, translates and optionally reviews subtitles through LM Studio, and provides a browser UI with resumable jobs.
- **Remote access** - Self-hosted [RustDesk](https://github.com/rustdesk/rustdesk) relay/rendezvous server.
- **Self-hosted PDF tools** - [Stirling PDF](https://github.com/Stirling-Tools/Stirling-PDF) for PDF management, OCR, conversion, and automated pipeline processing.

---

## Architecture

```text
Internet
  +-- Cloudflare Tunnel -> nginx-proxy-manager:80
  |                         |
  |                         +-- media / tools / n8n (Glance stays LAN-only)
  |                         `-- AIO-managed Apache on host:11000
  |
  `-- DNS-only hostnames from CLOUDFLARE_DDNS_DOMAINS
       `-- Router: WAN 80 -> host 80, WAN 443 -> host 4443
            `-- Nginx Proxy Manager
                 +-- host Jellyfin:8096
                 `-- open-webui:8080 -> host LM Studio:1234

Shared homelab Docker network
  +-- Browser -> host:7575 -> glance-live:8081 -> Glance:8080
  +-- glance-live -> qBittorrent sync API and host Jellyfin:8096
  +-- Glance -> jellyfin-counts-cache:8765 -> host Jellyfin:8096
  +-- AdGuard Home -> Unbound:5053 (requires local resolver config)
  `-- Subtitle Studio:8099 -> Whisper GPU + host LM Studio:1234

Isolated docker-monitoring network
  `-- Glance / Portracker / Uptime Kuma -> docker-proxy:2375 -> Docker socket

RustDesk HBBS/HBBR publish their own ports.
Nextcloud AIO -> separate Unix socket proxy -> Docker socket
Nextcloud AIO manages its own containers and networks.
```

The dashboard's host port `7575` belongs to `glance-live`; Glance itself stays on internal container port `8080`. `GLANCE_BIND_ADDRESS` defaults to loopback. For LAN clients, set it in the ignored root `.env` to the Docker host's LAN interface address and set `GLANCE_PUBLIC_URL` to the corresponding browser URL. This dashboard is unauthenticated and intended for the LAN: keep the bind private, firewall it to trusted LAN clients, and do not forward it from the Internet or add it to a public tunnel route. Do not change an Nginx Proxy Manager or tunnel route to publish this gateway. These settings do not verify the firewall or running services.

---

## Services

| Category | Service | Image | Port | Description |
|---|---|---|---|---|
| **Infrastructure** | Cloudflare Tunnel | `cloudflare/cloudflared` | - | Tunnel client for proxied Cloudflare routes |
| **Infrastructure** | Cloudflare DDNS | `favonia/cloudflare-ddns` | - | Updates the configured DNS-only records |
| **Infrastructure** | Nginx Proxy Manager | `jc21/nginx-proxy-manager` | 80, 81, 4443 | Reverse proxy and TLS; host 4443 maps to container 443 |
| **Infrastructure** | [Glance](https://github.com/glanceapp/glance) | `glanceapp/glance` | Internal 8080 | Responsive dashboard with seven navigation pages; served through the live gateway |
| **Infrastructure** | Glance live gateway | Local build from `glance/live/` | 7575 (host, configurable bind) | Same-origin dashboard proxy and server-side SSE relay for qBittorrent and Jellyfin widgets |
| **Infrastructure** | [Portracker](https://github.com/mostafa-wahied/portracker) | `mostafawahied/portracker` | 4999 | Docker container monitor |
| **Infrastructure** | [Docker Socket Proxy](https://github.com/Tecnativa/docker-socket-proxy) | `tecnativa/docker-socket-proxy` | Internal 2375 | Read-only Docker API for monitoring clients |
| **Infrastructure** | [Uptime Kuma](https://github.com/louislam/uptime-kuma) | `louislam/uptime-kuma` | 47028 | Uptime monitoring and alerting |
| **Infrastructure** | Jellyfin counts cache | `python:3.12-alpine` | - | Internal library-count helper for Glance on container port 8765 |
| **DNS** | [AdGuard Home](https://github.com/AdguardTeam/AdGuardHome) | `adguard/adguardhome` | 53, 3000, 8180 | Network-wide DNS filtering |
| **DNS** | [Unbound](https://github.com/MatthewVance/unbound-docker) | `mvance/unbound` | 5053 | Recursive DNS resolver |
| **Media** | Jellyfin | Separate host installation | 8096 | Media server outside this Compose stack; use the installer for your OS |
| **Media** | [qBittorrent](https://github.com/linuxserver/docker-qbittorrent) | `lscr.io/linuxserver/qbittorrent` | 8080, 6881 | Torrent downloader |
| **Media** | [Prowlarr](https://github.com/linuxserver/docker-prowlarr) | `lscr.io/linuxserver/prowlarr` | 9696 | Indexer manager |
| **Media** | [Sonarr](https://github.com/linuxserver/docker-sonarr) | `lscr.io/linuxserver/sonarr` | 8989 | TV show automation |
| **Media** | [Radarr](https://github.com/linuxserver/docker-radarr) | `lscr.io/linuxserver/radarr` | 7878 | Movie automation |
| **Media** | [Seerr](https://github.com/seerr-team/seerr) | `ghcr.io/seerr-team/seerr` | 5055 | Media discovery and requests |
| **Media** | [Bazarr](https://github.com/linuxserver/docker-bazarr) | `lscr.io/linuxserver/bazarr` | 6767 | Subtitle automation |
| **Media** | [Tdarr](https://github.com/HaveAGitGat/Tdarr) | `ghcr.io/haveagitgat/tdarr` | 8265, 8266 | GPU media transcoding |
| **Media** | [TubeArchivist](https://github.com/tubearchivist/tubearchivist) | `bbilly1/tubearchivist` | 8000 | YouTube archiving and management |
| **Media** | [MeTube](https://github.com/alexta69/metube) | `ghcr.io/alexta69/metube` | 8085 | YouTube downloader |
| **Media** | archivist-es / archivist-redis | `bbilly1/tubearchivist-es` / `redis` | - | TubeArchivist backends |
| **AI** | [Open WebUI](https://github.com/open-webui/open-webui) | `ghcr.io/open-webui/open-webui:cuda` | 3001 | Local LLM interface |
| **AI** | [SearxNG](https://github.com/searxng/searxng) | `searxng/searxng` | 8088 | Private metasearch engine |
| **Cloud** | Nextcloud Docker proxy | Local build from `docker-proxy/aio/` | Unix socket only | Separate Docker API proxy for AIO lifecycle operations |
| **Cloud** | [Nextcloud AIO](https://github.com/nextcloud/all-in-one) | `ghcr.io/nextcloud-releases/all-in-one` | 30917 (loopback) | Master interface; AIO provisions Apache separately on port 11000 |
| **Automation** | [n8n](https://github.com/n8n-io/n8n) | `n8nio/n8n` | 5678 | Workflow automation |
| **Subtitles** | Subtitle Studio | Local build from `subtitle-worker/` | 8099 | GPU transcription, local translation, and optional quality review |
| **Tools** | [IT-Tools](https://github.com/CorentinTh/it-tools) | `corentinth/it-tools` | 8888 | Developer tools |
| **Tools** | [Stirling PDF](https://github.com/Stirling-Tools/Stirling-PDF) | `stirlingtools/stirling-pdf` | 48537 | PDF tools |
| **Remote** | [RustDesk HBBS/HBBR](https://github.com/rustdesk/rustdesk-server) | `rustdesk/rustdesk-server` | 21115-21119 | Remote desktop rendezvous and relay |

---

## Compose Layout

The stack is split into smaller Compose files under `./compose/`, grouped by service role. The root `docker-compose.yml` includes every group. Ordinary integrations use `homelab`; Docker monitoring clients also join the isolated `docker-monitoring` network.

| File | Services |
|------|----------|
| `docker-compose.yml` | Compose entrypoint and service network definitions |
| `compose/infrastructure.yml` | Cloudflared, Cloudflare DDNS, Nginx Proxy Manager, Glance, Glance live gateway, Jellyfin counts cache, Portracker, Docker Socket Proxy, Uptime Kuma |
| `compose/dns.yml` | AdGuard Home, Unbound |
| `compose/media.yml` | qBittorrent, Prowlarr, Sonarr, Radarr, Seerr, Bazarr, Tdarr, MeTube, TubeArchivist, Redis, Elasticsearch |
| `compose/ai.yml` | Open WebUI, SearxNG |
| `compose/cloud-automation.yml` | n8n, Subtitle Studio, and its model/data volumes |
| `compose/nextcloud-aio.yml` | Nextcloud AIO master container, its Unix socket Docker proxy, and named volumes |
| `compose/tools.yml` | IT-Tools, Stirling PDF |
| `compose/remote.yml` | RustDesk HBBS/HBBR |

Services that declare `homelab` can use service names such as `nginx-proxy-manager`, `unbound`, and `archivist-redis`. Glance, Portracker, and Uptime Kuma reach the read-only Docker proxy through `docker-monitoring`. Nextcloud AIO manages its own child containers and persistent Docker volumes.

Docker build exclusions are scoped to each build context: the repository-root `.dockerignore` applies only when the repository root is the context, while `subtitle-worker/.dockerignore` and `docker-proxy/aio/.dockerignore` control their own service builds. The root ignore file keeps repository-only secrets, agent/CI metadata, caches, dependencies, and runtime data out of root-context builds; it does not change what Git tracks. Git ignores the local Compose override names listed below and root-anchored service data directories, while preserving `.env.example`, lockfiles, `subtitle-worker/web/app.css`, `.codex/`, and `.github/`. Adding an ignore rule does not untrack a file already committed.

Bind mounts beginning with `../` are relative to the included file in `compose/`, so application data is stored beside the root README. Most of those directories are intentionally ignored by Git. Jellyfin and LM Studio are host applications rather than Compose services.

## Component Guides

- [Glance dashboard](glance/README.md): seven pages, live qBittorrent/Jellyfin widgets, LAN binding, API keys, and the Jellyfin counts helper.
- [Subtitle Studio](subtitle-worker/README.md): setup, caching, cancellation and retries, translation reviews, API endpoints, and development checks.

---

## Prerequisites

Before you begin, ensure you have the following installed on your host machine:

| Requirement | Configuration | Link |
|---|---|---|
| Docker Desktop / Docker Engine | Linux containers; use the runtime for your OS in the platform guide | [Install Docker](https://docs.docker.com/get-docker/) |
| Docker Compose | Recent release supporting `include` and global overrides; the CPU override also uses `!reset` | [Include and overrides](https://docs.docker.com/compose/how-tos/multiple-compose-files/include/) |
| Git | Any | [Install Git](https://git-scm.com/downloads) |
| NVIDIA GPU support in Docker | Required for the original GPU configuration; use the non-GPU recipe otherwise | [NVIDIA container guide](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html) |
| LM Studio | Host server on port 1234 for AI and subtitle translation/review | [lmstudio.ai](https://lmstudio.ai/) |
| Jellyfin | Host server on port 8096 for playback and dashboard widgets | [Jellyfin downloads](https://jellyfin.org/downloads/server) |
| Cloudflare account + domain | Needed for tunnel and DDNS services | [Cloudflare dashboard](https://dash.cloudflare.com/) |

The base configuration reserves NVIDIA GPUs for Tdarr, Open WebUI, and Subtitle Studio. Those reservations apply on every OS unless you change the model or omit those services. Platform instructions below describe the required adaptations; they are not a claim that every image and service has been tested on every CPU architecture.

## Platform Guide

### Choose Your Runtime

| System | Container runtime | GPU path for this example | Media path example |
|---|---|---|---|
| Windows | Docker Desktop with Linux containers and WSL 2 | Supported NVIDIA GPU and Windows driver with WSL 2 GPU support | `D:/homelab-media` |
| macOS, Intel or Apple silicon | Docker Desktop | Use the non-GPU override; run Subtitle Studio natively on CPU | `/Users/your-user/homelab-media` |
| Linux | Docker Engine with the Compose plugin | NVIDIA driver plus NVIDIA Container Toolkit, or use the non-GPU override | `/srv/homelab-media` |

Docker Desktop's CUDA GPU passthrough to Linux containers uses the [Windows WSL 2 backend](https://docs.docker.com/desktop/features/gpu/). Apple silicon's GPU is not an NVIDIA CUDA device; running an `amd64` image through emulation does not provide CUDA access. For native Linux GPU containers, follow [NVIDIA's toolkit installation and Docker runtime configuration](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html).

On Apple silicon or Linux ARM64, check the architecture support of each image you select. For example, `docker buildx imagetools inspect <image:tag>` lists the image's published platforms. Select compatible images before startup; the checked-in Subtitle Studio Dockerfile uses an NVIDIA CUDA base, while its [native CPU instructions](subtitle-worker/README.md#native-cpu-setup-windows-macos-and-linux) avoid that base entirely.

Run Windows examples in PowerShell, and macOS/Linux examples in Bash or Zsh. When using a WSL terminal, use Linux-visible paths such as `/mnt/d/homelab-media`, rather than copying a PowerShell drive path. Single-line `docker compose` commands work in each shell. Native Python commands differ: use `python` on Windows and `python3` on macOS/Linux, or the interpreter in your virtual environment.

### Portable Paths and Local Overrides

Create `.env` using the [setup commands](#2-configure-environment-variables), then add an example media root suited to your OS:

| Variable | Windows | macOS | Linux |
|---|---|---|---|
| `EXAMPLE_MEDIA_ROOT` | `D:/homelab-media` | `/Users/your-user/homelab-media` | `/srv/homelab-media` |
| `SUBTITLE_LIBRARY_DIR` | `D:/homelab-media` | `/Users/your-user/homelab-media` | `/srv/homelab-media` |
| `SUBTITLE_MEDIA_DIR` | `D:/homelab-subtitles` | `/Users/your-user/homelab-subtitles` | `/srv/homelab-subtitles` |

`EXAMPLE_MEDIA_ROOT` is an additional variable for the override below, not a setting consumed by the checked-in base files. The override rejects a missing or empty value. Use real absolute paths; replace `your-user` with your username. Make `downloads`, `tv`, `movies`, `youtube`, `youtube-archive`, `youtube-cache`, and `transcode-cache` directories beneath that root. Grant the selected containers access to those directories. In Docker Desktop, make the directories available through its [file-sharing settings](https://docs.docker.com/desktop/settings-and-maintenance/settings/).

Save the following as `compose.override.yaml` in the repository root. It replaces media mounts by their container targets while retaining the services' config mounts:

```yaml
services:
  qbittorrent:
    volumes:
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/downloads:/downloads"
  sonarr:
    volumes:
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/tv:/tv"
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/downloads:/downloads"
  radarr:
    volumes:
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/movies:/media"
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/downloads:/downloads"
  bazarr:
    volumes:
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/movies:/movies"
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/tv:/tv"
  tdarr:
    volumes:
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/movies:/movies"
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/tv:/tv"
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/transcode-cache:/temp"
  metube:
    volumes:
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/youtube:/downloads"
  tubearchivist:
    volumes:
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/youtube-archive:/youtube"
      - "${EXAMPLE_MEDIA_ROOT:?Set EXAMPLE_MEDIA_ROOT in .env}/youtube-cache:/cache"
  nginx-proxy-manager:
    extra_hosts:
      - "host.docker.internal:host-gateway"
```

Docker documents [global overrides for included Compose files](https://docs.docker.com/compose/how-tos/multiple-compose-files/include/#using-overrides-with-included-compose-files). These common local override filenames are ignored by `.gitignore`: `compose.override.yaml`/`.yml` and `docker-compose.override.yaml`/`.yml`. Put private values in the ignored root `.env`, not in an override. Ignore rules do not untrack a file already committed to Git. Use the same file selection for validation and subsequent commands:

```shell
docker compose -f docker-compose.yml -f compose.override.yaml config --quiet
docker compose -f docker-compose.yml -f compose.override.yaml config
docker compose -f docker-compose.yml -f compose.override.yaml up -d nginx-proxy-manager it-tools
```

Inspect the rendered model locally to confirm host paths, retained config mounts, and hostnames. Its output can include credentials from `.env`. The last command starts two example services without GPU requirements; add others after preparing their configuration. If using this override, append the same `-f` flags to the later lifecycle commands in this guide. Alternatively, edit the corresponding base files directly for your deployment.

### Machines Without NVIDIA GPUs

For macOS, or Windows/Linux without NVIDIA, extend the same override under its existing `services` mapping. Add `profiles` to the existing `tdarr` entry, add the `subtitle-worker` and `open-webui` entries below, and retain the other path overrides. Do not create duplicate YAML service keys:

```yaml
services:
  tdarr:
    profiles: [nvidia]
    # Retain the volumes from your path override here.
  subtitle-worker:
    profiles: [nvidia]
  open-webui:
    image: ghcr.io/open-webui/open-webui:main
    deploy:
      resources:
        reservations:
          devices: !reset []
```

The `main` image is the [standard Open WebUI image](https://docs.openwebui.com/getting-started/quick-start/). Clearing its inherited device reservations removes the container's NVIDIA requirement; LM Studio's own acceleration is configured separately on the host. Compose's [`!reset` merge tag](https://docs.docker.com/reference/compose-file/merge/#reset-value) clears inherited settings rather than appending to them.

The profile excludes Tdarr and the CUDA subtitle container from ordinary startup. Explicitly naming either service still enables it, so use the [native CPU subtitle instructions](subtitle-worker/README.md#native-cpu-setup-windows-macos-and-linux) on machines without NVIDIA. After adapting the paths and validating the override, a non-GPU AI/tools example is:

```shell
docker compose -f docker-compose.yml -f compose.override.yaml config --quiet
docker compose -f docker-compose.yml -f compose.override.yaml up -d open-webui it-tools
```

This is a starting subset. Add DNS, media, tunnel, and cloud services only after supplying their settings and checking their image/platform support. A profile named `nvidia` is a hardware choice, not an OS choice; Linux and Windows can also use this non-GPU setup.

### Host Networking and Permissions

Docker Desktop provides [`host.docker.internal`](https://docs.docker.com/desktop/features/networking/networking-how-tos/#connect-a-container-to-a-service-on-the-host) for reaching host applications. On native Linux Engine, [`extra_hosts: ["host.docker.internal:host-gateway"]`](https://docs.docker.com/compose/how-tos/networking/#custom-dns-with-extra_hosts) maps that name to the host gateway. The base files already add it to Glance, the counts helper, Open WebUI, and the subtitle container; the override above adds it to Nginx Proxy Manager for Jellyfin and AIO proxy access.

On Linux, a host API bound only to `127.0.0.1` cannot normally be reached through the Docker bridge gateway. Bind Jellyfin or LM Studio to an address reachable from the containers and permit the needed container traffic through the host firewall. From a native CPU subtitle worker on the same host, use `http://127.0.0.1:1234/v1` instead of `host.docker.internal`. If Jellyfin runs in another container, attach it to the `homelab` network and adapt the proxy and dashboard targets to its service name.

Several media services set `PUID=1000` and `PGID=1000`. On Linux, use `id -u` and `id -g` to choose the intended owner's IDs, update those services' environment values, and grant that owner access to the bind mounts. Merely adding `PUID` or `PGID` to `.env` does not replace the hardcoded service values. On macOS/Windows, check Docker Desktop file access and image-specific permission behavior rather than assuming Linux ownership commands apply.

The Docker proxies and Portracker's `pid: host`, capabilities, and AppArmor setting refer to the Docker daemon's Linux environment. Under Docker Desktop, that is the Linux VM, so host metrics need not describe the physical Mac or Windows host. `DOCKER_SOCKET_SOURCE` defaults to `/var/run/docker.sock`; [rootless Docker](https://docs.docker.com/engine/security/rootless/tips/) and alternate daemon setups can use a different socket. Adapt the proxy socket settings using [Docker API access](#docker-api-access). Check for existing listeners on DNS/HTTP ports before starting AdGuard or Nginx Proxy Manager; binding the mapped ports must be supported by your runtime and host setup.

The base files also use fixed `container_name` values, a network named `homelab`, and an explicitly named Nextcloud volume. For two copies on the same Docker engine, adapt those names and published ports as well as the Compose project name; `docker compose -p another-name` alone does not isolate them.

---

## Setup Guide

### 1. Clone the Repository

```shell
git clone https://github.com/ACHRAF-YOUSSEF/homelab.git
cd homelab
```

### 2. Configure Environment Variables

Copy the example environment file and fill in your values.

Windows (PowerShell):

```powershell
if (-not (Test-Path -LiteralPath .env)) { Copy-Item .env.example .env }
```

macOS/Linux (Bash or Zsh):

```sh
if [ ! -f .env ]; then cp .env.example .env; fi
```

Run Compose commands from the repository root. The root file explicitly sets `env_file: .env` on each include to supply interpolation values. Create this file before validation or startup: passing `--env-file .env.example` alone does not replace the include declarations. This include setting does not inject the entire `.env` file into each container; services declare their container environment separately.

See the [Environment Variables Reference](#environment-variables-reference) and the grouped entries in [`.env.example`](.env.example). Configure shared defaults, public links, internal API URLs, Cloudflare domains/tokens, dashboard branding/feeds, TubeArchivist, n8n, and subtitle paths. Keep your actual domains, addresses, API keys, and personal links in the ignored `.env` file.

Use the [portable path override](#portable-paths-and-local-overrides) to supply your own absolute media directories for Windows, macOS, or Linux while retaining container targets such as `/downloads`, `/tv`, and `/media`. For the Docker subtitle worker, set `SUBTITLE_LIBRARY_DIR` to the library you want to expose and `SUBTITLE_MEDIA_DIR` to its writable export directory. The native subtitle recipe uses separate `*_ROOT` process environment variables.

### 3. Configure DNS (AdGuard Home + Unbound)

AdGuard Home uses Unbound as its upstream through the shared network at `unbound:5053`. The Compose file mounts `./unbound/` over `/opt/unbound/etc/unbound`, but that directory is ignored by Git and is not included in a fresh clone.

Before starting the DNS services, provide or restore a working Unbound configuration in `./unbound/` that listens on port `5053` and accepts queries from the Docker network. Configure recursive resolution, DNSSEC, and root hints as needed for your deployment; none of those settings can be inferred from the checked-in Compose file. See the [Unbound image documentation](https://github.com/MatthewVance/unbound-docker) for the expected files.

For custom local records, you can include an `a-records.conf` file from your Unbound configuration. Replace the documentation-only address below with the intended local service address:

```
# ./unbound/a-records.conf
local-data: "myservice.example. A 192.0.2.10"
```

**Add HaGeZi blocklists to AdGuard Home:**

In the AdGuard Home admin UI → Filters → DNS blocklists, add entries from [hagezi/dns-blocklists](https://github.com/hagezi/dns-blocklists). Recommended starting point:

```
# Multi PRO - Extended protection (Recommended)
https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.txt
```

> See the [DNS Blocklists section](#dns-blocklists) for more options.

AdGuard Home ports:

- Setup wizard: `http://<your-server-ip>:3000`
- Web UI after setup: `http://<your-server-ip>:8180`
- DNS: `<your-server-ip>:53` TCP/UDP
- HTTPS UI / DNS-over-HTTPS port mapping: `8443`
- DNS-over-TLS / DNS-over-QUIC / DNSCrypt ports: `853`, `784`, `8853`, `5443`

During setup, use `unbound:5053` as the upstream DNS server. Nginx Proxy Manager owns host ports `80` and `4443`, while AdGuard's HTTP and HTTPS container ports map to host `8180` and `8443`. The additional encrypted-DNS port mappings do not configure listeners or certificates automatically. AdGuard's state is stored in the ignored `./adguard/` directory.

### 4. Start the Stack

The commands below work in PowerShell, Bash, and Zsh. They omit explicit file flags; if you created a local override, use the same `-f` flags as in the platform guide to make your file selection explicit. Recent Compose also discovers `compose.override.yaml` automatically. Full startup requires the original NVIDIA setup or a suitable override and all selected service dependencies.

```sh
# Validate after creating .env and preparing local configuration
docker compose config --quiet
docker compose config --services

# Start configured services (the base configuration includes GPU services)
docker compose up -d --build

# Check status
docker compose ps

# Follow logs
docker compose logs -f
```

To restart a single service, for example Glance:

```shell
docker compose restart glance
```

To stop the entire stack:

```shell
docker compose down
```

To update all images and recreate containers:

```shell
docker compose pull
docker compose up -d --build
```

To start only one group, target services from that group directly:

```shell
docker compose up -d adguardhome unbound
docker compose up -d sonarr radarr prowlarr qbittorrent
```

On a configured NVIDIA host, Subtitle Studio can be built and started independently with `docker compose up -d --build subtitle-worker`. On macOS or a host without NVIDIA, use its [native CPU setup](subtitle-worker/README.md#native-cpu-setup-windows-macos-and-linux). Both serve the UI at `http://localhost:8099`; see the [component guide](subtitle-worker/README.md) for model downloads and translation setup. The app has no authentication. `SUBTITLE_BIND_ADDRESS` defaults the native bind and Compose host publication to loopback; change it only when you intend remote access and can restrict it to trusted clients.

`docker compose down` leaves named volumes and bind-mounted data in place. Subtitle Studio uses named `subtitle_models` and `subtitle_data` volumes; Nextcloud AIO uses `nextcloud_aio_mastercontainer` and the proxy socket volume `nextcloud_aio_docker_socket`, and manages additional volumes itself. Keep those volumes and the ignored application directories when moving or restoring the stack.

### 5. Configure Nginx Proxy Manager

1. Navigate to `http://<your-server-ip>:81`.
2. Complete the initial account setup required by the installed image.
3. Create proxy hosts for the internal services you want to expose. Keep Glance LAN-only; do not create an Internet-facing host for it. If an existing proxy is restricted to the LAN, its upstream must be `glance-live:8081` so SSE can stream through the same origin.
4. Request and attach TLS certificates through Nginx Proxy Manager.
5. For Jellyfin, use `host.docker.internal:8096` as the upstream and enable WebSockets.

LAN HTTPS access uses `https://<your-server-ip>:4443`. Container-to-container HTTPS still uses `nginx-proxy-manager:443`. Open WebUI's upstream is `open-webui:8080`; its published host port is `3001`.

> Docs: [Nginx Proxy Manager Documentation](https://nginxproxymanager.com/guide/)

### 6. Configure Jellyfin Direct Access

Cloudflare DDNS updates the domains listed in `CLOUDFLARE_DDNS_DOMAINS` using `CLOUDFLARE_API_TOKEN`. `PROXIED=false` leaves those records DNS-only. Set only the records you intend to expose directly; leave the domain-list variable blank when not using DDNS.

1. Configure your network so incoming traffic reaches the router serving the Docker host. If you have two routers, account for both forwarding layers.
2. Forward WAN TCP `80` to host TCP `80`, and WAN TCP `443` to host TCP `4443`, on the machine running Nginx Proxy Manager.
3. Create the Jellyfin proxy host in Nginx Proxy Manager, pointing at `host.docker.internal:8096`.
4. Request and attach a valid TLS certificate in Nginx Proxy Manager.
5. Keep `jellyfin.<your-domain>` gray-clouded in Cloudflare.

Traffic follows: `Internet -> your router/forwarding -> Nginx Proxy Manager -> Jellyfin`. Forwarding is independent of the host OS. For direct AI access, create the AI proxy host pointing to `open-webui:8080` and attach its TLS certificate. On native Linux, use the host-gateway mapping described in the platform guide for host-installed Jellyfin.

### 7. Configure Nextcloud AIO

Nextcloud AIO is defined in [compose/nextcloud-aio.yml](compose/nextcloud-aio.yml).
Its separate `nextcloud-docker-proxy` is built from
[docker-proxy/aio/Dockerfile](docker-proxy/aio/Dockerfile). The proxy
uses `network_mode: none` and exposes a Unix socket through the named volume
`nextcloud_aio_docker_socket`. AIO mounts that volume at `/var/run` and connects
to the proxy socket at `/var/run/docker.sock`; it does not mount the host socket.
The volume is writable because AIO also creates runtime sockets in `/var/run`.
Its `nocopy: true` mount option prevents Docker from copying AIO's `/var/run`
image contents over the ownership seeded by the proxy on a fresh volume.

For a fresh installation, start the proxy first to create the volume, then
inspect its daemon-side mount path. These commands work in PowerShell and Unix
terminals; include your local `-f` override flags if configured:

```shell
docker compose up -d --build nextcloud-docker-proxy
docker volume inspect nextcloud_aio_docker_socket --format '{{ .Mountpoint }}'
```

Set `AIO_PROXY_SOCKET_PATH` in `.env` to the returned mountpoint with
`/docker.sock` appended. Its default is
`/var/lib/docker/volumes/nextcloud_aio_docker_socket/_data/docker.sock`. This is
a path on the **Docker daemon's Linux filesystem**, including the Linux VM when
using Docker Desktop; it is not a Windows or macOS host path. A nondefault
Docker data root or rootless daemon may produce a different mountpoint. AIO
uses this daemon-side path for socket mounts in its update and HaRP/legacy proxy
containers, so those Docker clients also connect through the proxy. `DOCKER_SOCKET_SOURCE` separately selects the real daemon socket mounted
by the proxy.

Check the socket's group as seen inside the proxy container:

```shell
docker compose exec nextcloud-docker-proxy stat -c '%g' /var/run/docker.sock
```

Set `DOCKER_SOCKET_GID` to that numeric group in `.env` and recreate the proxy
if the value differs. Docker Desktop commonly reports `0`; native Linux often
uses the host socket's group, but rootless Docker and user-namespace mappings
can change the group seen inside the container. This in-container check is the
reliable value for the proxy. Do not change ownership or permissions on the
real daemon socket.

For a fresh installation, after updating `.env`, start AIO. For an existing
installation, follow [the one-time proxy socket migration](#existing-installation-migration)
first; that procedure starts the proxy and AIO master when it is complete.

```shell
docker compose up -d nextcloud-aio-mastercontainer
```

Open the management interface at `https://localhost:30917`; its host port is
bound to `127.0.0.1`. The master container is outside `homelab`. AIO creates its
own supporting containers, networks, and volumes.

The management proxy runs as UID `99`, GID `33`, with no Linux capabilities.
It uses `network_mode: none`, so it is isolated from Compose networks and has no
published TCP port; container loopback remains available. Its Unix socket is
mode `660`, owned by `99:33`, under a
mode `2770` proxy directory. `DOCKER_SOCKET_GID` adds the numeric group that can
read the daemon socket as seen inside the proxy container; Docker Desktop
commonly reports `0`. Use the in-container `stat` command above, especially with
rootless Docker, user namespaces, or a custom `DOCKER_SOCKET_SOURCE`. Do not
change ownership or permissions on the host daemon socket.

The management proxy permits the `BUILD`, `CONTAINERS`, `EXEC`, `IMAGES`, `INFO`,
`NETWORKS`, and `VOLUMES` endpoint groups, with `POST=1` for lifecycle writes.
Its client and server timeouts are one hour to accommodate long AIO shutdowns.
This access is separate from the read-only monitoring proxy. The full AIO
installation, backup, restore, and update lifecycle has not been tested with
this proxy configuration.

#### Existing installation migration

Migrate the proxy socket volume once after backing it up. First use **Stop containers** in the AIO interface and wait for
all children to stop. Then stop the master and proxy, rebuild the proxy, and
confirm the named volume exists:

```shell
docker compose stop nextcloud-aio-mastercontainer nextcloud-docker-proxy
docker compose build nextcloud-docker-proxy
docker volume inspect nextcloud_aio_docker_socket
```

Run this one-off ownership repair against only the proxy volume. It adjusts the
proxy directory and socket ownership/mode; it does not recursively change AIO's
PHP or runtime files:

```shell
docker run --rm --network none --user 0:33 --cap-drop ALL --cap-add CHOWN --cap-add FOWNER --security-opt no-new-privileges --mount type=volume,source=nextcloud_aio_docker_socket,target=/proxy --entrypoint sh tecnativa/docker-socket-proxy:v0.5.0 -c 'chown 99:33 /proxy && chmod 2770 /proxy && if [ -S /proxy/docker.sock ]; then chown 99:33 /proxy/docker.sock && chmod 660 /proxy/docker.sock; fi'
```

Start the rebuilt proxy and AIO master, wait for the proxy health check, then
use **Start containers** in AIO to recreate the children with the new proxy
socket:

```shell
docker compose up -d nextcloud-docker-proxy nextcloud-aio-mastercontainer
```
 AIO [recreates stopped children when starting them](https://github.com/nextcloud/all-in-one/blob/main/php/src/Controller/DockerController.php#L26-L44),
which reconnects their socket mounts. After a later proxy restart or recreation,
children with socket file mounts can retain an old socket inode; repeat the
AIO **Stop containers** → wait → **Start containers** sequence after the proxy
is healthy. Restarting a child alone retains its configured socket source.

`APACHE_PORT=11000` and `APACHE_IP_BINDING=0.0.0.0` configure the Apache endpoint
created by AIO after setup; the master does not directly publish that port.
For Nginx Proxy Manager, use a reachable host endpoint such as
`host.docker.internal:11000` and the platform guide's host-gateway mapping on
native Linux. Configure administrator credentials through AIO's setup interface.

### 8. Set Up Cloudflare Tunnel

The `tunnel` service runs Cloudflared using `CLOUDFLARE_TUNNEL_TOKEN`. Configure public routes in Cloudflare for the services you want to serve through the tunnel. Keep Glance out of public tunnel routes because it has no authentication and is intended for LAN access. Set the matching dashboard `*_PUBLIC_URL` values separately; these values do not create DNS or tunnel routes.

1. Go to [Cloudflare Zero Trust Dashboard](https://one.dash.cloudflare.com/).
2. Navigate to **Networks -> Tunnels -> Create a Tunnel**.
3. Choose **Cloudflared** and follow the setup wizard.
4. Copy the tunnel token into your `.env` file:
   ```env
   CLOUDFLARE_TUNNEL_TOKEN=your_token_here
   ```
5. In the tunnel's **Public Hostnames** tab, add routes pointing to Nginx Proxy Manager, for example `http://nginx-proxy-manager:80`.

> Docs: [Cloudflare Tunnel Documentation](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/get-started/)

---

## Environment Variables Reference

Create the root `.env` from [`.env.example`](.env.example) and customize its
example values. Keep actual addresses, domains, credentials, and personal links
in that ignored file. The template groups related settings rather than requiring
edits to deployment addresses in the dashboard YAML.

| Template group | Main settings and purpose |
| --- | --- |
| Host and browser service addresses | `TZ=Etc/UTC` and `HOMELAB_URL=http://localhost` are generic defaults. `*_PUBLIC_URL` values are browser destinations that can point to a LAN address or reverse proxy; setting one does not publish a service or create DNS/tunnel routes. `TUBEARCHIVIST_PUBLIC_URL` also configures TubeArchivist's `TA_HOST`. `N8N_HOST` and `N8N_WEBHOOK_URL` configure n8n's advertised address. |
| Glance LAN access and live updates | `GLANCE_BIND_ADDRESS` controls host publication of port `7575` and defaults to `127.0.0.1`; use the host's LAN interface address in private `.env` for LAN clients. Compose adds that address to the live gateway's host allowlist; add any LAN DNS hostname used in the browser to `GLANCE_LIVE_ALLOWED_HOSTS`. `GLANCE_LIVE_QBITTORRENT_INTERVAL` defaults to 2 seconds; `GLANCE_LIVE_JELLYFIN_INTERVAL` to 5 seconds; `GLANCE_LIVE_JELLYFIN_WS` enables Jellyfin WebSocket updates with REST reconciliation/fallback. Optional `QBITTORRENT_USERNAME` and `QBITTORRENT_PASSWORD` supply relay credentials; leave both blank when qBittorrent's network whitelist permits access. |
| Private credentials | Cloudflare tokens and `CLOUDFLARE_DDNS_DOMAINS`; Jellyfin, Sonarr, Radarr, and Seerr API keys; optional qBittorrent username/password for the live relay; TubeArchivist username/password and its Elasticsearch password. Fill only the integrations you use. |
| Docker API proxies | `DOCKER_SOCKET_SOURCE` selects the daemon-side socket mounted only by the proxies. `DOCKER_SOCKET_GID` selects the socket's numeric group as seen inside the unprivileged AIO proxy (Docker Desktop commonly reports `0`). `GLANCE_DOCKER_HOST` selects the monitoring proxy. `AIO_PROXY_SOCKET_PATH` identifies the daemon-side Nextcloud proxy socket used by AIO-managed child containers. See [Docker API access](#docker-api-access). |
| Internal service base URLs | `JELLYFIN_INTERNAL_URL`, the media API URLs, and `GLANCE_*_INTERNAL_URL` values are reachable from containers, with no trailing slash. They control dashboard checks, APIs, and the library-count helper independently of browser links. |
| Glance appearance | `GLANCE_APP_NAME`, `GLANCE_LOGO_TEXT`, and `GLANCE_WEATHER_LOCATION` replace example branding and weather. |
| Public external bookmarks and search providers | `GLANCE_*_URL` values customize external links and search shortcuts. |
| Glance public RSS feed sources | `GLANCE_*_FEED_URL` values customize RSS sources. |
| Media paths | `EXAMPLE_MEDIA_ROOT` supports the portable override example. `SUBTITLE_MEDIA_DIR` and `SUBTITLE_LIBRARY_DIR` choose the worker's writable output and read-only source mounts. Use absolute paths for your OS. |
| Subtitle models and local inference | `WHISPER_MODEL`, `WHISPER_LOCAL_FILES_ONLY`, `SUBTITLE_LLM_MODEL`, `SUBTITLE_REVIEW_MODEL`, `SUBTITLE_LLM_BASE_URL`, and `SUBTITLE_LLM_TIMEOUT` configure transcription and translation/review. Blank LLM model IDs select automatically. `SUBTITLE_BIND_ADDRESS` controls the native worker bind and Docker host publication; it defaults to loopback. See the [worker guide](subtitle-worker/README.md). |

`HOMELAB_URL` contains a scheme and host without a port or trailing slash; Glance
appends ports for shared-host links. A localhost public URL works for a browser
on the Docker host. Use reachable hostnames or addresses for other clients.
Public URL settings do not create DNS records, certificates, or tunnel routes.

After changing container environment variables, recreate the affected services
with `docker compose up -d --force-recreate <service-name>`. A plain restart
retains the old container environment. If using a local override, include the
same `-f docker-compose.yml -f compose.override.yaml` flags. Configure Nextcloud
and AdGuard through their setup interfaces; the template does not supply their
administrator credentials.

---

## Docker API Access

Docker clients use the proxies defined in Compose. Glance, Portracker, and
Uptime Kuma share the read-only `docker-proxy` on the isolated
`docker-monitoring` network. It listens on container port `2375` without a
published host port, permits GET requests to configured endpoint groups, and
rejects other HTTP methods with `POST=0`. Permitted container inspection, log,
and archive GET endpoints can expose environment values, logs, or files; give
proxy access only to trusted monitoring clients. Glance uses `GLANCE_DOCKER_HOST`;
Portracker uses `DOCKER_HOST=tcp://docker-proxy:2375`. Open WebUI has no Docker
API requirement in this stack and mounts no Docker socket.

Uptime Kuma stores Docker endpoints per Docker Host; a `DOCKER_HOST` environment
variable does not configure these monitors. Open `http://localhost:47028`, go to
**Settings → Docker Hosts**, and choose **Setup Docker Host**:

1. Give the host a name and set **Connection Type** to `tcp`.
2. Set **Docker Daemon** to `http://docker-proxy:2375`.
3. Choose **Test**, then **Save**, and select that Docker Host in each Docker
   Container monitor.

For an existing installation, edit each saved Unix socket host to use these TCP
settings. Editing the existing host keeps its monitor associations; creating a
new host requires selecting it in the affected monitors. Kuma's connection test
lists containers and its monitors inspect containers, so the proxy enables the
`CONTAINERS` endpoint group. See the [Uptime Kuma Docker Host implementation](https://github.com/louislam/uptime-kuma/blob/2.5.5/server/docker.js)
and [Docker monitor implementation](https://github.com/louislam/uptime-kuma/blob/2.5.5/server/model/monitor.js).

Nextcloud AIO uses a separate proxy for the lifecycle operations needed to create
and manage its child containers. See its [setup section](#7-configure-nextcloud-aio)
for the proxy socket and daemon-path configuration. Only the two proxy services
mount `DOCKER_SOCKET_SOURCE`; applications connect to a proxy endpoint.

---

## Port Reference

These are host ports unless explicitly marked AIO-managed. Compose mappings without a host address publish on all interfaces; `30917` is bound to loopback, and dashboard port `7575` defaults to loopback through `GLANCE_BIND_ADDRESS`. The read-only Docker proxy listens on container port `2375` only and has no published host port. Jellyfin's `8096` listener belongs to the separately installed host application.

| Port | Protocol | Service |
|------|----------|---------|
| 53 | TCP/UDP | AdGuard Home (DNS) |
| 80 | TCP | Nginx Proxy Manager (HTTP and ACME) |
| 81 | TCP | Nginx Proxy Manager (Admin UI) |
| 784 | UDP | AdGuard Home encrypted DNS mapping |
| 853 | TCP/UDP | AdGuard Home encrypted DNS mapping |
| 3000 | TCP | AdGuard Home first-run setup UI |
| 3001 | TCP | Open WebUI |
| 4443 | TCP | Nginx Proxy Manager HTTPS (container port 443) |
| 4999 | TCP | Portracker |
| 5053 | TCP/UDP | Unbound (DNS) |
| 5055 | TCP | Seerr |
| 5443 | TCP/UDP | AdGuard Home DNSCrypt mapping |
| 5678 | TCP | n8n |
| 6767 | TCP | Bazarr |
| 6881 | TCP/UDP | qBittorrent torrenting |
| 7575 | TCP | Glance live gateway (container port 8081; proxies Glance container port 8080). Bind defaults to loopback; set `GLANCE_BIND_ADDRESS` for LAN-only access. |
| 7878 | TCP | Radarr |
| 8000 | TCP | TubeArchivist |
| 8080 | TCP | qBittorrent Web UI |
| 8085 | TCP | MeTube (container port 8081) |
| 8088 | TCP | SearxNG |
| 8096 | TCP | Separately installed Jellyfin on the host |
| 8099 | TCP | Subtitle Studio |
| 8180 | TCP | AdGuard Home Web UI |
| 8265 | TCP | Tdarr Web UI |
| 8266 | TCP | Tdarr server |
| 8443 | TCP/UDP | AdGuard Home HTTPS and encrypted DNS endpoint |
| 8853 | UDP | AdGuard Home encrypted DNS mapping |
| 8888 | TCP | IT-Tools |
| 8989 | TCP | Sonarr |
| 9696 | TCP | Prowlarr |
| 11000 | TCP, AIO-managed | Nextcloud Apache endpoint after AIO setup |
| 21115-21119 | TCP | RustDesk rendezvous, relay, and WebSocket ports |
| 21116 | UDP | RustDesk |
| 30917 | TCP, loopback only | Nextcloud AIO management interface |
| 47028 | TCP | Uptime Kuma |
| 48537 | TCP | Stirling PDF |

---

## Media Stack Setup

The media stack follows a standard arr automation pipeline:

```
Seerr (requests) -> Sonarr/Radarr <- Prowlarr (indexers)
                       |
                       v
                 qBittorrent -> imported media
                                  +-- Bazarr (subtitles)
                                  +-- Tdarr (transcode)
                                  +-- Jellyfin (playback)
                                  `-- Subtitle Studio (manual local subtitles)
```

**Recommended setup order:**

1. **qBittorrent** - Configure download paths and categories (`tv`, `movies`).
2. **Prowlarr** - Add indexers and sync them to Sonarr/Radarr.
3. **Sonarr** - Set the TV root folder, quality profiles, and qBittorrent download client.
4. **Radarr** - Configure movie roots, profiles, and qBittorrent.
5. **Bazarr** - Connect Sonarr and Radarr for subtitle automation.
6. **Tdarr** - Point it at media libraries for GPU-accelerated transcoding.
7. **Seerr** - Connect Jellyfin, Sonarr, and Radarr for media requests.

Use Compose service names and container ports for connections between services, such as `qbittorrent:8080`, `sonarr:8989`, and `radarr:7878`. Sonarr sees TV files at `/tv`, Radarr sees movies at `/media`, and Bazarr sees movies at `/movies`; configure path mappings where an integration needs to translate those container paths. Jellyfin runs on the host and uses the host's media paths. Subtitle Studio writes to its separate output mount; copy exported SRTs beside the media when needed for playback.

### Jellyfin Plugins

These optional Jellyfin plugins can be configured separately. Jellyfin is outside Compose, and this repository does not install or configure them:

| Plugin | Description |
|--------|-------------|
| [ElegantFin](https://github.com/lscambo13/ElegantFin) | Custom theme for a cleaner, modern Jellyfin UI |
| [File Transformation](https://github.com/IAmParadox27/jellyfin-plugin-file-transformation) | Transforms media file paths before playback |
| [JavaScript Injector](https://github.com/n00bcodr/Jellyfin-JavaScript-Injector) | Injects custom JavaScript into the Jellyfin web client |
| [JellyFlare](https://github.com/MorganKryze/JellyFlare) | Displays a customisable announcement banner at the top of Jellyfin, with rotating messages, per-message scheduling, colour theming, click-through links, and a maintenance mode that blocks non-admin users |
| [Media Bar](https://github.com/IAmParadox27/jellyfin-plugin-media-bar) | Adds a persistent media bar to the Jellyfin UI |
| [Jellyfin Enhanced](https://github.com/n00bcodr/Jellyfin-Enhanced) | Collection of UI and UX enhancements for Jellyfin |
| [TubeArchivist Metadata](https://github.com/tubearchivist/tubearchivist-jf-plugin) | Adds TubeArchivist video and channel metadata, artwork, and library integration to Jellyfin |
| [YouTube Metadata](https://github.com/ankenyr/jellyfin-youtube-metadata-plugin) | Retrieves YouTube metadata for media in Jellyfin libraries |

---

## AI Stack Setup

### Open WebUI (Local LLM Interface)

Open WebUI is configured to connect to [LM Studio](https://lmstudio.ai/) running on the host machine:

- **LM Studio** must serve an API on host port `1234`; containers connect at `http://host.docker.internal:1234/v1`.
- Configure its listener and host firewall so the Docker containers can reach it, then select a model through LM Studio and Open WebUI.
- `compose/ai.yml` sets Open WebUI's API base URL and `OPENAI_API_KEY=lmstudio`. Subtitle Studio has a separate configurable base URL and its own model selectors.

> Docs: [Open WebUI Documentation](https://docs.openwebui.com/)

### SearxNG (Private Search Engine)

SearxNG exposes its UI on host port `8088`. Compose mounts `./searxng/` at `/etc/searxng` and sets a `SEARXNG_SETTINGS_URL`. The local directory is ignored by Git; a preconfigured `settings.yml` is not supplied in this checkout. Provide or restore local settings as needed, and use the service logs to confirm which configuration was loaded. Glance's Overview search bar uses `${HOMELAB_URL}:8088/search`; set `HOMELAB_URL` to an address reachable from your browser.

> Docs: [SearxNG Documentation](https://docs.searxng.org/)

---

## DNS Blocklists

This stack uses [AdGuard Home](https://adguard.com/en/adguard-home/overview.html) with DNS blocklists such as [HaGeZi DNS Blocklists](https://github.com/hagezi/dns-blocklists) for network-wide ad, tracker, and malware blocking.

### Recommended Lists for AdGuard Home

Add these URLs in **AdGuard Home → Filters → DNS blocklists**:

| List | URL | Description |
|------|-----|-------------|
| Multi PRO *(Recommended)* | `https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.txt` | Balanced protection — ads, tracking, telemetry, phishing, malware |
| Multi PRO++ | `https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/pro.plus.txt` | More aggressive — same as PRO + additional trackers |
| Threat Intelligence Feeds | `https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/tif.txt` | Blocks known malware, phishing, and C2 servers |
| Pop-Up Ads | `https://raw.githubusercontent.com/hagezi/dns-blocklists/main/adblock/popupads.txt` | Blocks annoying and malicious pop-up ads |

After adding lists, enable them in AdGuard Home and check the query log to confirm filtering is active.

> For the full list of available blocklists, see [hagezi/dns-blocklists](https://github.com/hagezi/dns-blocklists).

---

## Dashboard and Public URLs

Set browser destinations through the `*_PUBLIC_URL` group in [`.env.example`](.env.example), including `GLANCE_PUBLIC_URL`, `JELLYFIN_PUBLIC_URL`, `N8N_PUBLIC_URL`, `OPEN_WEBUI_PUBLIC_URL`, `NEXTCLOUD_PUBLIC_URL`, and `IT_TOOLS_PUBLIC_URL`. For Glance on other LAN devices, use a reachable LAN URL in your ignored `.env` and bind `GLANCE_BIND_ADDRESS` to the host's LAN interface. The dashboard has no authentication and is intended for LAN access; do not create a public tunnel route, WAN port forward, or externally reachable proxy host for it. The template uses generic local examples. Internal `*_INTERNAL_URL` values must be reachable from the service containers.

`HOMELAB_URL` supplies the common scheme/host used by dashboard links that append a service port. Public URLs and internal health checks are independent: a successful container check does not prove a public DNS, tunnel, or reverse-proxy route works. Keep personal deployment details in `.env` rather than in Markdown or shared YAML.

## Security Audits

The [security audit workflow](.github/workflows/security-audit.yml) runs on pushes, pull requests, manual dispatch, and a weekly schedule. Every job gates on failure; the workflow grants contents-read permission and no deployment credentials.

| Check | Coverage |
| --- | --- |
| Gitleaks | Current files and full reachable Git history; complete redaction, zero findings in the latest scan of 68 commits. |
| Repository privacy checks | 14 unit tests and the checker for private deployment data, credentials, and literal Glance endpoints; passed. |
| actionlint | GitHub Actions workflow syntax; pinned version 1.7.12 passed. |
| Bandit | Worker, shared HTTP client, model downloader, subtitle validator, Glance counts helper, and privacy checker Python sources at medium severity and confidence; zero findings. |
| pip-audit | Strict audit of resolved Python 3.12 dependencies; zero known vulnerabilities. |
| pnpm audit | Subtitle worker lockfile, including development dependencies, without installing project packages; zero moderate-or-higher findings. |
| Trivy | Filesystem dependency-manifest vulnerability and Dockerfile misconfiguration scan, including development dependencies; zero high or critical findings. Compose misconfiguration and supplied-image OS vulnerabilities are outside this scan. |

The latest full pinned audit passed every gate. See [SECURITY.md](SECURITY.md) for the initial findings, their remediation, scan limits, and deployment guidance. For task-focused repository help, global personal Codex agents and cross-platform setup are documented in [.codex/agents/README.md](.codex/agents/README.md). The role definitions live in the user's Codex home, so they can be reused across projects; cloning this repository does not install them.

The [component guides](#component-guides) document local proxy and service exposure.

---

## Development and Validation

After creating `.env`, validate the assembled configuration without starting containers. These commands work in PowerShell, Bash, and Zsh; include your local override flags when applicable:

```shell
docker compose config --quiet
docker compose config --services
git diff --check
```

The Glance YAML and assets are mounted read-only from `glance/config`; its [guide](glance/README.md) documents configuration changes and widget troubleshooting. Subtitle Studio's [guide](subtitle-worker/README.md) includes Python and Node tests and the command to rebuild the checked-in Tailwind CSS. Rebuild `subtitle-worker` after changing its Python or bundled web files because those files are copied into the image.

Application runtime data, API credentials, proxy hosts, certificates, router forwarding, LM Studio models, and Nextcloud's child containers are not fully represented by the tracked files. A successful Compose configuration check verifies the service definitions; use service logs and the component guides to verify your local deployment.

---

## Screenshots

> Screenshots coming soon. If you'd like to see a specific service, feel free to open an issue.

<!-- Add screenshots to the ./screenshots/ folder and reference them below -->
<!-- Example:
![Glance Dashboard](screenshots/glance.png)
![AdGuard Home Dashboard](screenshots/adguard.png)
![Open WebUI](screenshots/openwebui.png)
-->

---

## Projects Used

This homelab would not be possible without these amazing open-source projects:

| Project | GitHub |
|---------|--------|
| Cloudflared | [cloudflare/cloudflared](https://github.com/cloudflare/cloudflared) |
| Cloudflare DDNS | [favonia/cloudflare-ddns](https://github.com/favonia/cloudflare-ddns) |
| Nginx Proxy Manager | [NginxProxyManager/nginx-proxy-manager](https://github.com/NginxProxyManager/nginx-proxy-manager) |
| Glance | [glanceapp/glance](https://github.com/glanceapp/glance) |
| Glance Community Widgets | [glanceapp/community-widgets](https://github.com/glanceapp/community-widgets) |
| Portracker | [mostafa-wahied/portracker](https://github.com/mostafa-wahied/portracker) |
| Docker Socket Proxy | [Tecnativa/docker-socket-proxy](https://github.com/Tecnativa/docker-socket-proxy) |
| AdGuard Home | [AdguardTeam/AdGuardHome](https://github.com/AdguardTeam/AdGuardHome) |
| Unbound (Docker) | [MatthewVance/unbound-docker](https://github.com/MatthewVance/unbound-docker) |
| HaGeZi DNS Blocklists | [hagezi/dns-blocklists](https://github.com/hagezi/dns-blocklists) |
| qBittorrent (linuxserver) | [linuxserver/docker-qbittorrent](https://github.com/linuxserver/docker-qbittorrent) |
| Prowlarr (linuxserver) | [linuxserver/docker-prowlarr](https://github.com/linuxserver/docker-prowlarr) |
| Sonarr (linuxserver) | [linuxserver/docker-sonarr](https://github.com/linuxserver/docker-sonarr) |
| Radarr (linuxserver) | [linuxserver/docker-radarr](https://github.com/linuxserver/docker-radarr) |
| Seerr | [seerr-team/seerr](https://github.com/seerr-team/seerr) |
| Bazarr (linuxserver) | [linuxserver/docker-bazarr](https://github.com/linuxserver/docker-bazarr) |
| Jellyfin | [jellyfin/jellyfin](https://github.com/jellyfin/jellyfin) |
| Tdarr | [HaveAGitGat/Tdarr](https://github.com/HaveAGitGat/Tdarr) |
| TubeArchivist | [tubearchivist/tubearchivist](https://github.com/tubearchivist/tubearchivist) |
| MeTube | [alexta69/metube](https://github.com/alexta69/metube) |
| Open WebUI | [open-webui/open-webui](https://github.com/open-webui/open-webui) |
| SearxNG | [searxng/searxng](https://github.com/searxng/searxng) |
| Nextcloud AIO | [nextcloud/all-in-one](https://github.com/nextcloud/all-in-one) |
| n8n | [n8n-io/n8n](https://github.com/n8n-io/n8n) |
| Faster Whisper | [SYSTRAN/faster-whisper](https://github.com/SYSTRAN/faster-whisper) |
| Tailwind CSS | [tailwindlabs/tailwindcss](https://github.com/tailwindlabs/tailwindcss) |
| Stirling PDF | [Stirling-Tools/Stirling-PDF](https://github.com/Stirling-Tools/Stirling-PDF) |
| IT-Tools | [CorentinTh/it-tools](https://github.com/CorentinTh/it-tools) |
| RustDesk Server | [rustdesk/rustdesk-server](https://github.com/rustdesk/rustdesk-server) |
| Uptime Kuma | [louislam/uptime-kuma](https://github.com/louislam/uptime-kuma) |

---

## Star History

<a href="https://www.star-history.com/?repos=ACHRAF-YOUSSEF%2Fhomelab&type=timeline&legend=top-left">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=ACHRAF-YOUSSEF/homelab&type=timeline&theme=dark&legend=top-left" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=ACHRAF-YOUSSEF/homelab&type=timeline&theme=light&legend=top-left" />
    <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=ACHRAF-YOUSSEF/homelab&type=timeline&legend=top-left" />
  </picture>
</a>

---

---

<div align="center">

*If this repository helped you, consider leaving a star.*

</div>
