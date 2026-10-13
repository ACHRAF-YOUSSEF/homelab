# Subforge — Automatic Subtitle Conversion

**Subforge** is the name of the subtitle-conversion dashboard. The Compose service and folder remain `subtitle-converter`. It is an opt-in Rust watcher and local web dashboard that creates UTF-8 SubRip (`.srt`) sidecars from embedded subtitle tracks and supported subtitle files. FFmpeg handles common text tracks; TTML and SBV sidecars use the official [Subtitle Edit command-line converter](https://github.com/SubtitleEdit/subtitleedit/releases) even when OCR is disabled. Image-based subtitle tracks use Subtitle Edit with Tesseract OCR only when OCR is explicitly enabled. The runtime image is built locally with a pinned Rust builder and a Debian Bookworm runtime; it does not include Python.

## Start the service

Set the four `CONVERTER_*_DIR` values in the ignored root `.env` to the host folders used for your movie and TV libraries. The `.env.example` values are examples for the current Compose layout; replace them with paths on your machine. On Windows with Docker Desktop, use Windows drive paths shared with Docker Desktop. On macOS and Linux, use absolute host paths such as `/Volumes/media/movies` or `/srv/media/movies`.

Start the service and its `subtitle-conversion` Compose profile from the repository root:

```shell
docker compose --profile subtitle-conversion up -d --build subtitle-converter
```

Open the responsive Subforge dashboard at `http://localhost:8100`. Routes are bookmarkable: reloading the page and using browser back/forward preserves the selected page and Settings section. On narrow screens, the navigation becomes a slide-out menu, the dashboard cards and settings stack vertically, and the subtitle inventory remains horizontally scrollable. The dashboard always requires HTTP Basic Authentication with username `admin`. If `CONVERTER_UI_PASSWORD` is blank, the service creates a random password once and stores it in `/data/ui-password` with mode `0600`; the password is not written to logs. Retrieve it from PowerShell, Bash, or Zsh with:

```shell
docker compose --profile subtitle-conversion exec subtitle-converter cat /data/ui-password
```

Alternatively, set `CONVERTER_UI_PASSWORD` in the ignored root `.env` to a private password of at least 12 characters. The named `subtitle_converter_data` volume preserves the generated password, jobs, settings, preferences, and scan state across container rebuilds. The Rust runtime reads the existing JSON state/settings/preferences/history files and password, preserving compatible data when the image is upgraded; Jellyfin connection details remain managed through environment variables.

### Troubleshoot startup

If the service exits during startup and its logs identify `CONVERTER_UI_PASSWORD` as invalid, set it privately in the ignored root `.env` to a strong printable password of 12–256 characters, or leave it blank to use the persistent generated password. Recreate only this service:

```shell
docker compose --profile subtitle-conversion up -d --build subtitle-converter
```

When using the generated password, retrieve it with the command above. Startup validation reports the setting and phase without printing its value; never paste credentials into logs or shared documentation.

Compose publishes container port `8100` on host port `8100`, bound to `127.0.0.1` by default through `CONVERTER_BIND_ADDRESS`. The service joins `homelab` so the dashboard can reach configured Sonarr, Radarr, Bazarr, and Jellyfin servers and receive Sonarr/Radarr webhooks. Add the hostname used in your browser to `CONVERTER_ALLOWED_HOSTS` when you use a non-local URL; retain the default entries. The host allowlist complements the bind setting and firewall. Keep Basic Authentication traffic on loopback or a trusted private network; use HTTPS through a trusted reverse proxy for remote browser access.

The breadcrumb and refresh button stay fixed at the top while scrolling. On desktop, the small chevron at the bottom of the sidebar collapses or expands navigation; the preference is remembered in that browser. On mobile, a separate hamburger button in the header opens and closes the navigation drawer.

Docker needs network access while building the pinned Rust application and OCR tool. The running service uses the `homelab` network for its web UI and configured integrations. It has no GPU reservation or Docker socket. The process runs as UID/GID 1000 with a read-only container root, dropped capabilities, and `no-new-privileges`; it writes generated SRTs into the mounted libraries and private state into `/data`. The runtime image contains the Rust executable and media tools, not Python. The bounded HTTP server has four request workers and a 16-connection queue; the job queue is limited to 32 and one conversion worker runs one media-tool child at a time.

The dashboard's Overview shows conversion progress, queue, recent activity, and live logs. **Library → Refresh inventory** lists files and subtitle tracks without changing media; select an item to extract tracks with the selected languages. Search matches filenames and full paths case-insensitively and combines with the root and media-type filters. The inventory displays 25 rows per page by default; choose 25, 50, or 100 rows, then use Previous/Next or the page selector to navigate. Changing search, filters, or page size returns to page one; background status refreshes preserve the current page when possible. Inspections return at most 2,000 files, and the Library page identifies a capped result so you can inspect a smaller root. **Scan and extract** queues automatic conversion for the chosen roots. The Activity page shows queued, running, and completed jobs and lets you cancel queued or running work. Settings stores default languages in the data volume; **Settings → General** shows whether OCR is enabled for the running service. OCR is controlled by environment configuration, not a dashboard toggle.

Overview's Recent activity, Quick extraction, and Live log panels; the Library inventory; Activity's Jobs list; and the settings cards for languages, hooks, root folders, Sonarr, Radarr, Bazarr, and Jellyfin can be collapsed. The browser remembers each panel's open or collapsed state. Job failures, per-file conversion causes, integration notifications, and refresh results appear in a collapsed disclosure grouped by origin, provider, and repeated message, with a count and a preview; opening it shows bounded examples and, when a provider is identified, a shortcut to its settings. Jobs that finish processing files with failures are marked **Completed with errors**. The disclosure's open state and scroll position are preserved during status refreshes. Integration diagnostics give a safe HTTP or connection category without exposing API keys or raw server responses; they explain errors but do not repair an integration configuration.

### Glance status widget

The Glance **Media** page includes a Subforge custom API widget. It displays the converted-file count, queue length, configured scan-root count, recent failures, current run and progress, and selected languages. It only reads an aggregate from Subforge's `/api/widget` endpoint; the response contains no media paths and the widget cannot start or change jobs. Glance refreshes the widget every 15 seconds. The widget uses the copied [Subforge SVG icon](../glance/config/assets/subforge.svg) in the dashboard service list.

The widget is optional and disabled while `CONVERTER_WIDGET_TOKEN` is blank. To enable it, create a private random token between 32 and 256 characters and set the same value for `CONVERTER_WIDGET_TOKEN` in the ignored root `.env`; never put it in `.env.example`, YAML, or shared documentation. The Compose file passes this dedicated token only to Glance and Subforge. It is separate from the dashboard password, webhook token, and media-server API keys. `SUBFORGE_PUBLIC_URL` controls the link opened in a browser and defaults to `http://localhost:8100`; `GLANCE_SUBFORGE_INTERNAL_URL` is Glance's container-to-container base URL and defaults to `http://subtitle-converter:8100`.

Subforge remains in the opt-in `subtitle-conversion` Compose profile. A token does not start or enable that profile. Start the service explicitly as described above. After changing the token, recreate both containers so each receives the same value:

```shell
docker compose --profile subtitle-conversion up -d --force-recreate glance subtitle-converter
```

After changing only either URL, recreate Glance. A `404` status means the widget endpoint is disabled; `401` means the tokens do not match. A connection error usually means Subforge is stopped or `GLANCE_SUBFORGE_INTERNAL_URL` cannot reach it on the Compose network.

## What it converts

The watcher scans movie and TV folders recursively for supported video files (`.mkv`, `.mks`, `.mp4`, `.m4v`, `.webm`, `.mov`, `.avi`, `.ts`, `.m2ts`) and sidecars (`.ass`, `.ssa`, `.vtt`, `.ttml`, `.smi`, `.sami`, `.sbv`, `.sup`, `.sub`, `.idx`). It exports each supported embedded subtitle track and converts sidecars to SRT beside the source. Existing SRT files are not inputs, and generated files never replace existing files. For example, an embedded English track may produce `Example.en.srt`; duplicate or unknown tracks receive track-number suffixes, and forced or hearing-impaired flags are retained in the name when available. Empty, correctly timed subtitle cues are removed from the converter's staged output before validation. A malformed SRT still fails validation and is not published. If a source later changes, an existing SRT is still left as-is; the converter does not refresh or overwrite it automatically.

Common text tracks keep their subtitle language and are converted with FFmpeg. TTML and SBV sidecars use the Subtitle Edit fallback converter; this path runs with OCR disabled too, because it converts text formats. Image-based tracks use Tesseract OCR through Subtitle Edit only when `CONVERTER_ENABLE_OCR=true`; OCR is disabled by default and the **Settings → General** screen shows whether the running service has OCR permission. The OCR language is inferred from embedded track metadata, a VobSub IDX language section, or a language suffix in a sidecar filename. The image OCR language packs are installed in the container. If the language cannot be identified, `CONVERTER_OCR_LANGUAGE` supplies the fallback and defaults to `eng`; OCR does not detect a language by recognizing the subtitle image itself. Use filenames such as `Example.fr.sup` when an image sidecar has no other language metadata. Language metadata and available OCR language packs affect image conversion; codecs or formats that the tools cannot read can still fail.

OCR takes longer than exporting text tracks and can misread names, punctuation, or stylized text. Review OCR output before relying on it. SRT cannot preserve ASS styling, positioning, or karaoke effects. Subtitle Edit warnings and conversion failures appear in the service logs; the watcher keeps source files and existing SRTs. It waits until an input's modification age reaches the configured minimum, retries failed inputs after the configured retry interval, and records successful fingerprints in its persistent state volume. If the state volume is removed, inputs are scanned and processed again; existing output SRTs remain protected.

The converter stages each output beside its source, removes empty timed cues in that staging area, validates the completed SRT, then publishes it without replacing a destination. The source and any existing SRT remain untouched if conversion or validation fails. On Linux, publication uses a same-filesystem hard link; filesystems that do not support that operation will fail the conversion and leave the source untouched. A source that changes during conversion is not published.

## Configuration

The Compose service lets you change the four host-side library folders through `.env`; the container destinations are fixed by the standard Compose file and define the available media mounts:

| Variable | Host folder mounted at | Default |
|---|---|---|
| `CONVERTER_MOVIES_DIR` | `/movies` | `D:/film` |
| `CONVERTER_MOVIES_C_DIR` | `/movies-C` | `C:/film` |
| `CONVERTER_TV_DIR` | `/tv` | `D:/tv show` |
| `CONVERTER_TV_C_DIR` | `/tv-C` | `C:/tv show` |
| `CONVERTER_SCAN_INTERVAL` | Seconds between scans | `300` |
| `CONVERTER_MIN_AGE` | Minimum file modification age before conversion, in seconds | `120` |
| `CONVERTER_TIMEOUT` | Maximum runtime for an individual conversion operation, in seconds | `7200` |
| `CONVERTER_RETRY_INTERVAL` | Delay before retrying a failed unchanged input, in seconds | `3600` |
| `CONVERTER_MEMORY_LIMIT` | Container memory ceiling; 512 MiB is a text-only option | `1536m` |
| `CONVERTER_ENABLE_OCR` | Enable OCR for image subtitle tracks; requires a compatible memory ceiling | `false` |
| `CONVERTER_OCR_LANGUAGE` | Tesseract fallback language for unknown image-subtitle language | `eng` |
| `CONVERTER_BIND_ADDRESS` | Host interface for the dashboard publication | `127.0.0.1` |
| `CONVERTER_ALLOWED_HOSTS` | Comma-separated `Host` header allowlist; preserve the defaults and add your browser hostname if needed | `localhost,127.0.0.1,::1,subtitle-converter` |
| `CONVERTER_UI_PASSWORD` | Dashboard password (12–256 printable characters); blank generates and saves one in `/data/ui-password` | Blank |
| `CONVERTER_WEBHOOK_TOKEN` | Shared Sonarr/Radarr webhook token; blank disables the hook endpoints | Blank |
| `BAZARR_INTERNAL_URL` | Bazarr URL reachable from the converter on `homelab` | `http://bazarr:6767` |
| `BAZARR_API_KEY` | Bazarr API key; keep it private in `.env` | Blank |

For example, to use `/srv/media/movies` as the first movie library on Linux, set `CONVERTER_MOVIES_DIR=/srv/media/movies` in `.env`; the container will still see it at `/movies`. On Windows, use a drive path available to Docker Desktop, such as the example `D:/film`. The mounted host folders must be writable by container UID/GID 1000 so the converter can create SRT sidecars. Keep them aligned with Bazarr and media-manager access. In Subforge, **Settings → Media Management** lets you select which mounted folders or subfolders to scan; add, browse, scan, or remove roots there. This selection is saved immediately and applies to future scans. Folder browsing is restricted to the configured mounts. Changing a `CONVERTER_*_DIR` changes the host folder behind its corresponding mount; changing container-side destinations instead requires updating the Compose mount and matching `server.py --root` arguments. The `subtitle_converter_data` named volume mounted at `/data` separately persists the generated dashboard password, settings, selected scan roots, job history, and scan state across rebuilds. `.env.example` keeps API-key entries blank; store real service keys and any chosen dashboard or webhook passwords only in the ignored `.env`.

## Media server connections

The Settings page stores Sonarr, Radarr, and Bazarr URLs, API keys, and path mappings in a private file under `/data`. The browser receives only whether each API key is set; it never receives the saved key. Environment variables prefill the Arr and Bazarr connection details; Arr values can also be adjusted in Settings. Bazarr uses `BAZARR_INTERNAL_URL` (default `http://bazarr:6767`) and `BAZARR_API_KEY`; leave the API key blank in `.env.example` and keep the real value private in the ignored root `.env`. Bazarr is opt-in in Settings. Use **Test connection** after configuring each server. API failures are summarized using safe status/category messages; for example, HTTP 401/403 means the service rejected the configured credentials or access. After conversion, enabled Arr connections receive scan requests for matching media items. Path mappings translate paths in Arr webhook payloads into paths mounted in this container:

| Arr reports | Converter sees | Notes |
|---|---|---|
| Radarr `/media` | `/movies` | Add a Radarr path mapping in the dashboard. |
| Radarr `/media-C` | `/movies-C` | Add this mapping when using the second movie mount. |
| Sonarr `/tv` | `/tv` | Matches the first TV mount in Compose. |
| Sonarr `/tv-C` | `/tv-C` | Matches the second TV mount in Compose. |

If your Arr path differs, add a mapping under that server's Settings tab. Sonarr and Radarr webhooks require a shared token. Set `CONVERTER_WEBHOOK_TOKEN` in the ignored `.env`, then in each app's **Settings → Connect**, add a Webhook targeting `http://subtitle-converter:8100/hooks/sonarr` or `http://subtitle-converter:8100/hooks/radarr`. See the [Sonarr](https://wiki.servarr.com/sonarr/settings#connect) or [Radarr](https://wiki.servarr.com/radarr/settings#connect) connection guide. In the connection's advanced headers, add `X-Webhook-Token` with the same token. Enable file import/download, upgrade, and rename events, then use the app's **Test** action. The webhook accepts the Test event without queueing a job; supported media events queue an extraction. If a newly imported file is younger than `CONVERTER_MIN_AGE`, that attempt remains pending for the next background scan.

For Bazarr, enable the integration and test the connection in **Settings → Bazarr**. The converter collects newly created SRT paths during a conversion job and sends one batch when the job finishes. It matches those paths to existing Bazarr movies or series and requests a scoped database sync and disk scan for the affected items. The converter requests indexing/sync actions; Bazarr may also apply its own subtitle search and download rules during a sync, depending on your Bazarr configuration. Use **Sync and scan library** for a manual refresh: Subforge queues Bazarr's available series/movie synchronization and full subtitle-index scan tasks, skipping tasks Bazarr reports as already running. Bazarr may apply its configured search/download rules during those global sync tasks as well.

Path mappings are required whenever Bazarr reports media paths differently from the converter's mounted paths. For example, if Bazarr sees `/media/Movies` while Subforge sees `/movies`, add the Bazarr mapping `/media/Movies` → `/movies`; add a separate mapping for TV if needed. Confirm the mapped roots refer to the same host files and that both containers can access them. Subforge does not assume a mapping based on folder names, and automatic scoped scans report no match if the paths cannot be reconciled. On Windows, map the server-reported path to the corresponding container path (such as `/movies`), rather than entering a host drive path as the local container path. On macOS/Linux, use the same container-path rule.

Jellyfin uses the existing `JELLYFIN_INTERNAL_URL` and `JELLYFIN_API_KEY` environment variables; the dashboard displays the connection as environment-managed and does not save it in its settings file. Requests use Jellyfin's current MediaBrowser authorization header with the API key, supporting recent Jellyfin servers. Enable the integration, test the connection, load libraries, select movie and series libraries, and enable the matching refresh toggles. New SRT files then trigger refreshes only within those selected library locations. **Refresh timing** chooses an immediate local metadata refresh or a filesystem update notification. If immediate lookup cannot identify the media item, the service sends a scoped filesystem notification instead. Use **Refresh selected libraries** to enumerate and refresh movie and episode items inside the selected libraries; the job supports cancellation and stops with a visible error at 20,000 items per library. These requests use Jellyfin's [item refresh API](https://github.com/jellyfin/jellyfin/blob/master/Jellyfin.Api/Controllers/ItemRefreshController.cs). HTTP 401/403 can indicate an unsupported authorization format, rejected/expired API key, or insufficient access. Verify the configured server URL and API key and review the safe connection diagnostic before changing credentials. The settings page cannot edit or store the Jellyfin key; after changing `JELLYFIN_API_KEY` in the ignored root `.env`, recreate the converter container so it receives the new value.

If Jellyfin uses different paths, add mappings on its Settings tab. A native Windows path such as `D:\Media\Movies` can map to the converter's `/movies` mount; `C:\Media\TV` can map to `/tv-C`. This also translates the newly created SRT paths back into the paths Jellyfin sees.

### Safe integration diagnostic

The Rust executable includes a read-only connection check for configured media integrations. Run it from the repository root with the service running:

```shell
docker compose --profile subtitle-conversion exec subtitle-converter subforge integration-check bazarr
```

Replace `bazarr` with `sonarr`, `radarr`, or `jellyfin` to check another integration. The diagnostic uses the converter's configured credentials, prints only the provider, connection result, and—for Jellyfin—library/selection counts. It does not print server addresses, library names, API keys, or raw server responses. A nonzero exit status means the configured connection check failed; use the dashboard's safe diagnostic and service logs for next steps. This check verifies connectivity only and does not change server settings or queue scans.

## Memory use and OCR

Compose and `.env.example` give Subforge a `1536m` memory ceiling by default while keeping image OCR disabled. To enable image-track extraction, set `CONVERTER_ENABLE_OCR=true` in the private `.env` and recreate the service; the default ceiling already matches the OCR configuration. When OCR remains disabled, a text-only deployment can set `CONVERTER_MEMORY_LIMIT=512m`, but TTML and SBV use Subtitle Edit even without OCR, so validate the limit against your subtitle formats. The General settings screen reports whether OCR is enabled; it does not change the environment-controlled permission. Subforge limits the OCR library to one OpenMP thread and runs one conversion child at a time. These are maximums for the container, not memory reserved on the host; Docker uses less when the workload needs less.

See the repository's [memory budget guide](../docs/memory-budget.md) for the other example workload ceilings, Nextcloud AIO child limits, and a dry-run-first, reversible helper. Measure actual peak use under your own workload and leave room for the host, Docker Desktop or the Docker daemon, and host applications such as LM Studio.

## Run the Rust application natively

For native development, install Rust and Cargo, FFmpeg/`ffprobe`, and MKVToolNix/`mkvmerge`. Install Subtitle Edit `seconv` v5.2.0 and Tesseract with the needed trained data only if you enable OCR. The Docker image bundles the required Linux tools, but native installs are managed by the host operating system. See [FFmpeg downloads](https://ffmpeg.org/download.html), [MKVToolNix downloads](https://mkvtoolnix.download/downloads.html), the [Subtitle Edit releases](https://github.com/SubtitleEdit/subtitleedit/releases), and [Tesseract documentation](https://tesseract-ocr.github.io/).

PowerShell example with two Windows media roots and persistent application data:

```powershell
$subforgeData = Join-Path $env:LOCALAPPDATA 'Subforge'
New-Item -ItemType Directory -Force -Path $subforgeData | Out-Null
cargo run --manifest-path .\subtitle-converter\Cargo.toml -- --host 127.0.0.1 --data $subforgeData --root 'D:\Media\Movies' --root 'D:\Media\TV'
```

On macOS/Linux, use native paths and a user state directory:

```shell
subforge_data="${XDG_STATE_HOME:-$HOME/.local/state}/subforge"
mkdir -p "$subforge_data"
cargo run --manifest-path subtitle-converter/Cargo.toml -- --host 127.0.0.1 --data "$subforge_data" --root /srv/media/movies --root /srv/media/tv
```

The native server scans periodically by default; pass `--no-auto-scan` to disable scheduled scans. Set environment variables such as `CONVERTER_ENABLE_OCR`, `CONVERTER_OCR_LANGUAGE`, and the media-server integration URLs/keys in the shell environment before running. Keep API keys private.

## Development checks

Run the Rust test suite from the repository root:

```shell
cargo test --locked --manifest-path subtitle-converter/Cargo.toml
```

Build the production binary without updating locked dependencies:

```shell
cargo build --locked --release --manifest-path subtitle-converter/Cargo.toml
```

OCR coverage and third-party media tools can vary by codec, language data, and filesystem; successful Rust tests do not establish behavior for every media file or deployment.

The legacy Python implementation and its fixture tests remain in the repository as reference material while Rust parity checks continue; they are not part of the runtime image. Native Rust development requires the tools listed above and permission to create temporary files next to each input so completed SRTs can be published without overwriting existing files.
