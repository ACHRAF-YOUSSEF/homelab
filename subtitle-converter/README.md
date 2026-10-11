# Subforge — Automatic Subtitle Conversion

**Subforge** is the name of the subtitle-conversion dashboard. The Compose service and folder remain `subtitle-converter`. It is an opt-in watcher and local web dashboard that creates UTF-8 SubRip (`.srt`) sidecars from embedded subtitle tracks and supported subtitle files, using the official [Subtitle Edit command-line converter](https://github.com/SubtitleEdit/subtitleedit/releases) and Tesseract for image tracks. The container is built locally from the pinned converter release.

## Start the service

Set the four `CONVERTER_*_DIR` values in the ignored root `.env` to the host folders used for your movie and TV libraries. The `.env.example` values are examples for the current Compose layout; replace them with paths on your machine. On Windows with Docker Desktop, use Windows drive paths shared with Docker Desktop. On macOS and Linux, use absolute host paths such as `/Volumes/media/movies` or `/srv/media/movies`.

Start the service and its `subtitle-conversion` Compose profile from the repository root:

```shell
docker compose --profile subtitle-conversion up -d --build subtitle-converter
```

Open the responsive Subforge dashboard at `http://localhost:8100`. On narrow screens, the navigation becomes a slide-out menu, the dashboard cards and settings stack vertically, and the subtitle inventory remains horizontally scrollable. The dashboard always requires HTTP Basic Authentication with username `admin`. If `CONVERTER_UI_PASSWORD` is blank, the service creates a random password once and stores it in `/data/ui-password` with mode `0600`; the password is not written to logs. Retrieve it from PowerShell, Bash, or Zsh with:

```shell
docker compose --profile subtitle-conversion exec subtitle-converter cat /data/ui-password
```

Alternatively, set `CONVERTER_UI_PASSWORD` in the ignored root `.env` to a private password of at least 12 characters. The named `subtitle_converter_data` volume preserves the generated password, jobs, settings, and scan state across container rebuilds.

### Troubleshoot startup

If the service exits during startup and its logs identify `CONVERTER_UI_PASSWORD` as invalid, set it privately in the ignored root `.env` to a strong printable password of 12–256 characters, or leave it blank to use the persistent generated password. Recreate only this service:

```shell
docker compose --profile subtitle-conversion up -d --build subtitle-converter
```

When using the generated password, retrieve it with the command above. Startup validation reports the setting and phase without printing its value; never paste credentials into logs or shared documentation.

Compose publishes container port `8100` on host port `8100`, bound to `127.0.0.1` by default through `CONVERTER_BIND_ADDRESS`. The service joins `homelab` so the dashboard can reach configured Sonarr, Radarr, and Jellyfin servers and receive their webhooks. Add the hostname used in your browser to `CONVERTER_ALLOWED_HOSTS` when you use a non-local URL; retain the default entries. The host allowlist complements the bind setting and firewall. Keep Basic Authentication traffic on loopback or a trusted private network; use HTTPS through a trusted reverse proxy for remote browser access.

The build downloads Subtitle Edit's Linux `seconv` v5.2.0 release and checks its SHA-256 digest for `amd64` or `arm64`. Docker needs network access while building, and the running service uses the `homelab` network for its web UI and configured integrations. It has no GPU reservation or Docker socket. The process runs as UID/GID 1000 with a read-only container root, dropped capabilities, and `no-new-privileges`; it writes generated SRTs into the mounted libraries and private state into `/data`.

The dashboard's Overview shows conversion progress, queue, recent activity, and live logs. **Library → Inspect** lists files and subtitle tracks without changing media; select an item to extract tracks with the selected languages. **Scan library** queues automatic conversion for the chosen roots. The Activity page shows queued, running, and completed jobs and lets you cancel queued or running work. Settings stores default languages in the data volume.

## What it converts

The watcher scans movie and TV folders recursively for supported video files (`.mkv`, `.mks`, `.mp4`, `.m4v`, `.webm`, `.mov`, `.avi`, `.ts`, `.m2ts`) and sidecars (`.ass`, `.ssa`, `.vtt`, `.ttml`, `.smi`, `.sami`, `.sbv`, `.sup`, `.sub`, `.idx`). It exports each supported embedded subtitle track and converts sidecars to SRT beside the source. Existing SRT files are not inputs, and generated files never replace existing files. For example, an embedded English track may produce `Example.en.srt`; duplicate or unknown tracks receive track-number suffixes, and forced or hearing-impaired flags are retained in the name when available. If a source later changes, an existing SRT is still left as-is; the converter does not refresh or overwrite it automatically.

Text tracks keep their subtitle language. Image-based tracks use Tesseract OCR with the language inferred from embedded track metadata, a VobSub IDX language section, or a language suffix in a sidecar filename. The image OCR language packs are installed in the container. If the language cannot be identified, `CONVERTER_OCR_LANGUAGE` supplies the fallback and defaults to `eng`; OCR does not detect a language by recognizing the subtitle image itself. Use filenames such as `Example.fr.sup` when an image sidecar has no other language metadata. Language metadata and available OCR language packs affect image conversion; codecs or formats that Subtitle Edit cannot read can still fail.

OCR takes longer than exporting text tracks and can misread names, punctuation, or stylized text. Review OCR output before relying on it. SRT cannot preserve ASS styling, positioning, or karaoke effects. Subtitle Edit warnings and conversion failures appear in the service logs; the watcher keeps source files and existing SRTs. It waits until an input's modification age reaches the configured minimum, retries failed inputs after the configured retry interval, and records successful fingerprints in its persistent state volume. If the state volume is removed, inputs are scanned and processed again; existing output SRTs remain protected.

The converter stages each output beside its source, validates the completed SRT, then publishes it without replacing a destination. On Linux, publication uses a same-filesystem hard link; filesystems that do not support that operation will fail the conversion and leave the source untouched. A source that changes during conversion is not published.

## Configuration

The Compose service mounts each host directory at a separate container path:

| Variable | Purpose | Default |
|---|---|---|
| `CONVERTER_MOVIES_DIR` | First movie library host path | See `.env.example` |
| `CONVERTER_MOVIES_C_DIR` | Second movie library host path | See `.env.example` |
| `CONVERTER_TV_DIR` | First TV library host path | See `.env.example` |
| `CONVERTER_TV_C_DIR` | Second TV library host path | See `.env.example` |
| `CONVERTER_SCAN_INTERVAL` | Seconds between scans | `300` |
| `CONVERTER_MIN_AGE` | Minimum file modification age before conversion, in seconds | `120` |
| `CONVERTER_TIMEOUT` | Maximum runtime for an individual conversion operation, in seconds | `7200` |
| `CONVERTER_RETRY_INTERVAL` | Delay before retrying a failed unchanged input, in seconds | `3600` |
| `CONVERTER_OCR_LANGUAGE` | Tesseract fallback language for unknown image-subtitle language | `eng` |
| `CONVERTER_BIND_ADDRESS` | Host interface for the dashboard publication | `127.0.0.1` |
| `CONVERTER_ALLOWED_HOSTS` | Comma-separated `Host` header allowlist; preserve the defaults and add your browser hostname if needed | `localhost,127.0.0.1,::1,subtitle-converter` |
| `CONVERTER_UI_PASSWORD` | Dashboard password (12–256 printable characters); blank generates and saves one in `/data/ui-password` | Blank |
| `CONVERTER_WEBHOOK_TOKEN` | Shared Sonarr/Radarr webhook token; blank disables the hook endpoints | Blank |

The mounted library directories must be writable by container UID/GID 1000 so the converter can create SRT sidecars. Keep the host paths and container mounts aligned with Bazarr and your media manager roots. To use different folders, change only the applicable `CONVERTER_*_DIR` values in your private `.env`; do not put private deployment paths in shared documentation. `.env.example` also contains blank API-key entries for the Arr applications and Jellyfin; keep real keys and any chosen dashboard or webhook passwords in the ignored `.env`.

## Media server connections

The Settings page stores Sonarr and Radarr URLs, API keys, and path mappings in a private file under `/data`. The browser receives only whether each API key is set; it never receives the saved key. The values from `SONARR_INTERNAL_URL`, `SONARR_API_KEY`, `RADARR_INTERNAL_URL`, and `RADARR_API_KEY` prefill the Arr connections; you can adjust them in Settings. Use the **Test connection** button after configuring each server. After conversion, enabled Arr connections receive a scan request for matching media items. Path mappings translate paths in Arr webhook payloads into paths mounted in this container:

| Arr reports | Converter sees | Notes |
|---|---|---|
| Radarr `/media` | `/movies` | Add a Radarr path mapping in the dashboard. |
| Radarr `/media-C` | `/movies-C` | Add this mapping when using the second movie mount. |
| Sonarr `/tv` | `/tv` | Matches the first TV mount in Compose. |
| Sonarr `/tv-C` | `/tv-C` | Matches the second TV mount in Compose. |

If your Arr path differs, add a mapping under that server's Settings tab. Sonarr and Radarr webhooks require a shared token. Set `CONVERTER_WEBHOOK_TOKEN` in the ignored `.env`, then in each app's **Settings → Connect**, add a Webhook targeting `http://subtitle-converter:8100/hooks/sonarr` or `http://subtitle-converter:8100/hooks/radarr`. See the [Sonarr](https://wiki.servarr.com/sonarr/settings#connect) or [Radarr](https://wiki.servarr.com/radarr/settings#connect) connection guide. In the connection's advanced headers, add `X-Webhook-Token` with the same token. Enable file import/download, upgrade, and rename events, then use the app's **Test** action. The webhook accepts the Test event without queueing a job; supported media events queue an extraction. If a newly imported file is younger than `CONVERTER_MIN_AGE`, that attempt remains pending for the next background scan.

Jellyfin uses the existing `JELLYFIN_INTERNAL_URL` and `JELLYFIN_API_KEY` environment variables; the dashboard displays the connection as environment-managed and does not save it in its settings file. Enable the integration, test the connection, load libraries, select movie and series libraries, and enable the matching refresh toggles. New SRT files then trigger refreshes only within those selected library locations. **Refresh timing** chooses an immediate local metadata refresh or a filesystem update notification. If immediate lookup cannot identify the media item, the service sends a scoped filesystem notification instead. Use **Refresh selected libraries** to enumerate and refresh movie and episode items inside the selected libraries; the job supports cancellation and stops with a visible error at 20,000 items per library. These requests use Jellyfin's [item refresh API](https://github.com/jellyfin/jellyfin/blob/master/Jellyfin.Api/Controllers/ItemRefreshController.cs).

If Jellyfin uses different paths, add mappings on its Settings tab. A native Windows path such as `D:\Media\Movies` can map to the converter's `/movies` mount; `C:\Media\TV` can map to `/tv-C`. This also translates the newly created SRT paths back into the paths Jellyfin sees.

## Run once or preview

The container runs repeated scans by default. The standalone script supports `--once` to scan and convert once, or `--dry-run` to list inputs that would be processed without writing files. Repeat `--language CODE` to limit extraction languages; omitting it selects all supported tracks. For native use, install Python 3, Subtitle Edit's `seconv` v5.2.0, FFmpeg/`ffprobe`, MKVToolNix/`mkvmerge`, Tesseract with the needed trained data, and (for full language metadata mapping) the `iso-codes` data package. Then run the script with one or more `--root` options for actual library paths. The script itself uses only the Python standard library. See the [Subtitle Edit releases](https://github.com/SubtitleEdit/subtitleedit/releases), [Tesseract documentation](https://tesseract-ocr.github.io/), [FFmpeg downloads](https://ffmpeg.org/download.html), and [MKVToolNix downloads](https://mkvtoolnix.download/downloads.html) for platform-specific installers.

The script's default state path is `/data/state.json`, which is writable in the container because `/data` is the persistent state volume. Native runs should pass `--state` with a writable state-file path.

For example, PowerShell can preview and then process two explicit roots:

```powershell
$converterState = Join-Path $env:LOCALAPPDATA 'subtitle-converter\state.json'
python .\subtitle-converter\converter.py --root 'D:\Media\Movies' --root 'D:\Media\TV' --state $converterState --dry-run
python .\subtitle-converter\converter.py --root 'D:\Media\Movies' --root 'D:\Media\TV' --state $converterState --once
```

On macOS or Linux, use `python3` and absolute paths:

```shell
converter_state="${XDG_STATE_HOME:-$HOME/.local/state}/subtitle-converter/state.json"
python3 subtitle-converter/converter.py --root /srv/media/movies --root /srv/media/tv --state "$converter_state" --once
```

Without `--once` or `--dry-run`, the script repeats scans using `CONVERTER_SCAN_INTERVAL`.

The native language map is limited if the operating system does not provide `/usr/share/iso-codes/json/iso_639-2.json`; install its `iso-codes` package or use `CONVERTER_OCR_LANGUAGE` for the fallback OCR language.

## Development checks

Run the converter's Python fixture tests from the repository root:

```shell
python -m unittest discover -s subtitle-converter -p 'test_*.py'
```

The security audit workflow also builds the pinned converter image and runs its integration fixtures with networking disabled and no host media mounted. Those fixtures cover text extraction and local OCR; they do not verify every media codec, language, or platform filesystem.

Set `CONVERTER_SECONV`, `CONVERTER_FFPROBE`, and `CONVERTER_MKVMERGE` if the tools are not available on `PATH`. Native deployments should also set `CONVERTER_OCR_LANGUAGE` for unknown-language image subtitles. The native process must be able to create temporary files next to each input so completed SRTs can be published without overwriting existing files.
