# Subforge — Automatic Subtitle Conversion

**Subforge** is the name of the subtitle-conversion dashboard. The Compose service and folder remain `subtitle-converter`. It is a Rust watcher and local web dashboard that creates UTF-8 SubRip (`.srt`) sidecars from embedded subtitle tracks and supported subtitle files. FFmpeg handles common text tracks; TTML and SBV sidecars use the official [Subtitle Edit command-line converter](https://github.com/SubtitleEdit/subtitleedit/releases) even when OCR is disabled. Image-based subtitle tracks use Subtitle Edit with Tesseract OCR only when OCR is explicitly enabled. The CPU image is a locally built Rust edition on a Debian Bookworm runtime and contains no Python. The optional CUDA edition compiles the same app with its CUDA feature and adds the separately locked Python/PaddleOCR helper; its dashboard and API report the compiled edition.

## Start the service

Compose requires the ignored root `.env` for interpolation. Create it from `.env.example` if it does not exist, then set the four `CONVERTER_*_DIR` values to host folders used for your movie and TV libraries. The sample paths match the current Compose layout; replace them with paths on your machine. On Windows with Docker Desktop, use drive paths shared with Docker Desktop. On macOS and Linux, use absolute host paths such as `/Volumes/media/movies` or `/srv/media/movies`. The dashboard binds to loopback by default; `CONVERTER_ALLOWED_HOSTS` defaults to the local names and service name. `CONVERTER_UI_PASSWORD` may remain blank to generate a persistent password. Media-service keys are optional; keep any real keys in the ignored `.env`. Direct `docker run` does not read Compose's `.env`; supply equivalent runtime settings and all four media bind mounts plus persistent `/data` yourself.

Build the selected edition locally before starting it. Compose only starts a prebuilt tag: it neither builds nor pulls Subforge, and startup fails if the selected local tag is missing.

```shell
docker build --target final --tag homelab-subtitle-converter:0.4.0-cpu subtitle-converter
```

PowerShell uses the same local build command:

```powershell
docker build --target final --tag homelab-subtitle-converter:0.4.0-cpu subtitle-converter
```

Then start the prebuilt CPU image from the repository root:

```shell
docker compose up -d --no-deps subtitle-converter
```

Open the responsive Subforge dashboard at `http://localhost:8100`. Routes are bookmarkable: reloading the page and using browser back/forward preserves the selected page and Settings section. On narrow screens, the navigation becomes a slide-out menu, the dashboard cards and settings stack vertically, and the subtitle inventory remains horizontally scrollable. The dashboard always requires HTTP Basic Authentication with username `admin`. **Settings → Status**, the sidebar, and the header identify whether the running binary is the compiled CPU-only or CUDA edition; environment variables cannot change this build-time edition. If `CONVERTER_UI_PASSWORD` is blank, the service creates a random password once and stores it in `/data/ui-password` with mode `0600`; the password is not written to logs. Retrieve it from PowerShell, Bash, or Zsh with:

```shell
docker compose exec subtitle-converter cat /data/ui-password
```

Alternatively, set `CONVERTER_UI_PASSWORD` in the ignored root `.env` to a private password of at least 12 characters. The named `subtitle_converter_data` volume preserves the generated password, jobs, settings, preferences, and scan state across container rebuilds. The Rust runtime reads the existing JSON state/settings/preferences/history files and password, preserving compatible data when the image is upgraded; Jellyfin connection details remain managed through environment variables.

The library inventory is saved separately in `/data/inventory.json`, so it survives page reloads, container restarts, and job-history rotation. Search, the Issues/Has tracks/Pending OCR/Failed/Ready/Metadata and media/root filters, page size and selected page survive reloads in the same browser tab. An inspection keeps the previous inventory visible until it finishes; cancelling it keeps that saved snapshot. Startup does not automatically build an inventory or convert files. Use **Library → Inspect library** to create or refresh the snapshot, which is limited to 2,000 files and stored with private permissions and bounded metadata; status polling sends only its summary, and the browser fetches the rows when the snapshot changes.

### Dashboard workflow

Version 0.4.0 uses a dark Ember-style palette and Tdarr-inspired work-queue density. The Overview pipeline has clickable **Scan**, **Inspect**, **Convert**, and **Ready** stages that open related Library views, plus a live progress bar for the current job. **Convert** shows only the number of files pending OCR; its sublabel separately shows queued-job count. **Ready** counts distinct generated SRT outputs recorded in Subforge's state, excluding external SRT files. If the bounded 20,000-output ledger cannot provide an exact total, it reports the count as unavailable. Separately, the Overview and Glance totals count input files with a tracked completed conversion pass, not generated SRTs; completed text passes remain counted if later OCR fails.

Open an inventory row's **Details** drawer to inspect and select individual embedded or sidecar tracks. The drawer uses unambiguous IDs for embedded subtitle streams and VobSub tracks; unsupported or ambiguous IDs stay disabled until reinspection. A lone sidecar source can be selected without a stream ID. Image tracks are selectable only when OCR is permitted by the service environment. The drawer shows the OCR permission state and sends the selected IDs with the extraction request. Job cards offer **Retry failed** for recorded failures, bounded to the first 64 paths when a job has more; a single-path retry also retains its selected track IDs. Automatic discovery and scheduled OCR defer unchanged failures unless a manual retry is requested. Scoped failure records follow the current file fingerprint; a successful whole-file pass clears only selections it fully covers, leaving unresolved track-specific failures intact.

Integration status dots summarize the latest observed connection test or recorded integration request. They are not live health checks, and Subforge does not poll each provider in the background. Jellyfin library selectors display library names with their IDs as helper text; up to 100 loaded names and IDs are cached in the current tab session so selectors can be restored without another lookup. The Settings → Status page includes live CPU, RAM, worker-capacity, and cache-usage bars; unavailable measurements remain unavailable rather than being estimated.

The Glance widget reports failures from the latest conversion scan, rather than adding repeated failures from old jobs. Historical activity remains available for diagnosis. The integration settings explain each provider's automation and show its latest recorded request; successful connection tests alone do not register Sonarr or Radarr import webhooks.

Unix media tools run with a fixed UTF-8 locale so non-ASCII Matroska titles cannot truncate their JSON output. Inspection falls back to FFprobe when MKVToolNix returns unusable metadata, and mismatched native track tables are discarded rather than paired with the wrong subtitle streams. Valid alternative SRT timestamp separators and hour padding are canonicalized; damaged cue framing still prevents publication. Conversion failures identify the track or stream, language, cue number and reason without logging subtitle text. Successful unchanged conversions stay cached. Automatic discovery and scheduled OCR defer unchanged failures. Job cards include **Retry failed** for recorded failed paths; the **Include unchanged failed tracks in the next manual OCR run** option explicitly retries recorded OCR failures. **Convert selected** may also include OCR for the selected file when deployment permission enables it. Run **Inspect library** again to replace a snapshot containing older inspection errors.

### Troubleshoot startup

If the service exits during startup and its logs identify `CONVERTER_UI_PASSWORD` as invalid, set it privately in the ignored root `.env` to a strong printable password of 12–256 characters, or leave it blank to use the persistent generated password. Recreate only this service:

```shell
docker compose up -d --no-deps subtitle-converter
```

For a CUDA deployment, keep `-f compose/subtitle-converter.cuda.yml` in the command so troubleshooting does not switch the service to the CPU image.

When using the generated password, retrieve it with the command above. Startup validation reports the setting and phase without printing its value; never paste credentials into logs or shared documentation.

Compose publishes container port `8100` on host port `8100`, bound to `127.0.0.1` by default through `CONVERTER_BIND_ADDRESS`. The service joins `homelab` so the dashboard can reach configured Sonarr, Radarr, Bazarr, and Jellyfin servers and receive Sonarr/Radarr webhooks. Add the hostname used in your browser to `CONVERTER_ALLOWED_HOSTS` when you use a non-local URL; retain the default entries. The host allowlist complements the bind setting and firewall. Keep Basic Authentication traffic on loopback or a trusted private network; use HTTPS through a trusted reverse proxy for remote browser access.

The breadcrumb and refresh button stay fixed at the top while scrolling. On desktop, the small chevron at the bottom of the sidebar collapses or expands navigation; the preference is remembered in that browser. On mobile, a separate hamburger button in the header opens and closes the navigation drawer.

Docker needs network access while building the pinned Rust application and OCR tool. The running service uses the `homelab` network for its web UI and configured integrations and has no Docker socket. The default CPU image does not reserve GPU devices. The process runs as UID/GID 1000 with a read-only container root, dropped capabilities, and `no-new-privileges`; it writes generated SRTs into the mounted libraries and private state into `/data`. The default CPU runtime image contains the Rust executable and media tools, not Python. The bounded HTTP server has four request workers and a 16-connection queue; the job queue is limited to 32. File-level conversion runs within the bounded allocation described below.

The dashboard's Overview shows conversion progress, queue, recent activity, and live logs. **Library → Inspect library** lists files and subtitle tracks without changing media; open a row and use **Convert selected** for chosen tracks. Search matches filenames and full paths case-insensitively and combines with the root and media-type filters. The inventory displays 25 rows per page by default; choose 25, 50, or 100 rows, then use Previous/Next or the page selector to navigate. Changing search, filters, or page size returns to page one; background status refreshes preserve the current page when possible. Inspections return at most 2,000 files, and the Library page identifies a capped result so you can inspect a smaller root. **Convert all** queues conversion for the chosen roots. The Activity page shows queued, running, and completed jobs and lets you cancel queued or running work. Settings stores default languages in the data volume; **Settings → General** shows whether OCR is enabled for the running service. OCR permission remains environment controlled, while **Settings → Workers** allocates CPU file workers and CPU OCR slots within the deployment and current resource limits.

### Scheduled tasks

**Settings → Tasks** exposes the fixed discovery, OCR, inventory, health, cleanup, and backup tasks. It shows each task's enabled state, interval, next run, last run, status, and result. Changes and schedule-pause state persist in `/data/tasks.json`. There is no special startup conversion run; fresh schedules set their first run relative to service startup, while saved schedules keep their persisted next-run time. The task coordinator processes queued jobs in order; within a file-based job, independent files can run concurrently up to the worker limits in **Settings → Workers**. Scheduled task pause and worker-pool pause are separate settings.

| Task | Default interval | Minimum interval | Default state and behavior |
|---|---:|---:|---|
| Library discovery | 6 hours | 6 hours | Enabled; finds supported files and performs automatic text conversion. |
| Subtitle OCR | 1 day | 6 hours | Enabled only when `CONVERTER_ENABLE_OCR=true`; processes recorded new or changed inputs with OCR pending. It does not walk the library. |
| Media inventory | 1 day | 6 hours | Disabled; enable or run it manually to refresh the Library snapshot. |
| Health check | 6 hours | 1 hour | Enabled; checks configured mount and state-directory availability locally, with no network polling. |
| Cleanup | 1 day | 1 day | Enabled; removes stale conversion-state and metadata-cache entries only. |
| Backup | 7 days | 1 day | Enabled; writes a private backup of application state, retaining three generations. It does not include media files. |

Task intervals can be adjusted in Settings → Tasks subject to each task's displayed minimum; the maximum is 30 days. **Pause automatic schedules** persists across restarts. It does not cancel a running job, and manual Run actions remain available while paused. On first startup, `CONVERTER_SCAN_INTERVAL` sets the discovery interval; values below 21,600 seconds are clamped to six hours, and Compose defaults it to `21600`. After that, the saved interval in `tasks.json` takes precedence. Worker allocation and worker-pool pause are stored separately in `/data/workers.conf`; pausing workers stops dispatching new files and lets active files finish. The metadata cache is capped at 32 MiB and 5,000 entries. Each tool process stays single-threaded; independent file workers provide bounded parallelism. Compose defaults to a 1 CPU and 1536 MiB memory ceiling; the text-only memory option is 512 MiB. Worker resource budgets are soft estimates, not reserved or guaranteed memory; see [Memory use and OCR](#memory-use-and-ocr) for details.

The runtime, CLI `--version`, API, and dashboard report the compiled package version from `Cargo.toml`; **Settings → Status** and the sidebar show `v0.4.0`. Version is release metadata, not an editable setting or environment variable. Patch fixes advance the patch version (for example, `0.2.0` → `0.2.1` → `0.2.2`); features advance the minor version. For each revision, update `Cargo.toml` and the root package entry in `Cargo.lock`, the fixed Dockerfile image-version label, and the fixed `homelab-subtitle-converter:0.4.0-cpu` and `homelab-subtitle-converter:0.4.0-cuda` references in the Compose files together. The edition is compiled into each image; the CPU edition cannot be switched to CUDA by setting environment variables. Rebuilding the same revision keeps its version. Old `#/system/tasks` and `#/system/status` bookmarks redirect to their unified Settings sections. See the [revision history](CHANGELOG.md) for the changes in each version. The Overview and Glance totals count input files with a tracked completed conversion pass, not generated SRT files. Completed text passes remain counted when later OCR fails, including text passes that queued image tracks without creating a text SRT. The Overview pipeline Ready count is separate: it counts distinct SRT outputs recorded in conversion state, excludes externally existing SRT files, and becomes unavailable if the bounded output ledger cannot provide an exact count.

Overview's Recent activity, Quick extraction, and Live log panels; the Library inventory; Activity's Jobs list; and the settings cards for languages, hooks, root folders, Sonarr, Radarr, Bazarr, and Jellyfin can be collapsed. The browser remembers each panel's open or collapsed state. Job failures, per-file conversion causes, integration notifications, and refresh results appear in a collapsed disclosure grouped by origin, provider, and repeated message, with a count and a preview; opening it shows bounded examples and, when a provider is identified, a shortcut to its settings. Jobs that finish processing files with failures are marked **Completed with errors**. The disclosure's open state and scroll position are preserved during status refreshes. Integration diagnostics give a safe HTTP or connection category without exposing API keys or raw server responses; they explain errors but do not repair an integration configuration.

### Glance status widget

The Glance **Media** page includes a Subforge custom API widget. It displays the processed-file count, queue length, configured scan-root count, recent failures, current run and progress, and selected languages. It only reads an aggregate from Subforge's `/api/widget` endpoint; the response contains no media paths and the widget cannot start or change jobs. Glance refreshes the widget every 15 seconds. The widget uses the copied [Subforge SVG icon](../glance/config/assets/subforge.svg) in the dashboard service list.

The widget is optional and disabled while `CONVERTER_WIDGET_TOKEN` is blank. To enable it, create a private random token between 32 and 256 characters and set the same value for `CONVERTER_WIDGET_TOKEN` in the ignored root `.env`; never put it in `.env.example`, YAML, or shared documentation. The Compose file passes this dedicated token only to Glance and Subforge. It is separate from the dashboard password, webhook token, and media-server API keys. `SUBFORGE_PUBLIC_URL` controls the link opened in a browser and defaults to `http://localhost:8100`; `GLANCE_SUBFORGE_INTERNAL_URL` is Glance's container-to-container base URL and defaults to `http://subtitle-converter:8100`.

Subforge is part of the regular Compose stack and starts after its selected local image has been built. The widget token only enables the read-only widget endpoint. After changing the token, recreate both containers so each receives the same value:

```shell
docker compose up -d --force-recreate glance subtitle-converter
```

For a CUDA deployment, add `-f compose/subtitle-converter.cuda.yml` after
`docker-compose.yml` to both this command and the CUDA service start command.

After changing only either URL, recreate Glance. A `404` status means the widget endpoint is disabled; `401` means the tokens do not match. A connection error usually means Subforge is stopped or `GLANCE_SUBFORGE_INTERNAL_URL` cannot reach it on the Compose network.

## What it converts

The watcher scans movie and TV folders recursively for supported video files (`.mkv`, `.mks`, `.mp4`, `.m4v`, `.webm`, `.mov`, `.avi`, `.ts`, `.m2ts`) and sidecars (`.ass`, `.ssa`, `.vtt`, `.ttml`, `.smi`, `.sami`, `.sbv`, `.sup`, `.sub`, `.idx`). It exports each supported embedded subtitle track and converts sidecars to SRT beside the source. Existing SRT files are not inputs, and generated files never replace existing files. For example, an embedded English track may produce `Example.en.srt`; duplicate or unknown tracks receive track-number suffixes, and forced or hearing-impaired flags are retained in the name when available. Empty, correctly timed subtitle cues are removed from the converter's staged output before validation. A malformed SRT still fails validation and is not published. If a source later changes, an existing SRT is still left as-is; the converter does not refresh or overwrite it automatically.

Common text tracks keep their subtitle language and are converted with FFmpeg. TTML and SBV sidecars use the Subtitle Edit fallback converter; this path runs with OCR disabled too, because it converts text formats. Image-based tracks use Tesseract OCR through Subtitle Edit only when `CONVERTER_ENABLE_OCR=true`; OCR is disabled by default and the **Settings → General** screen shows whether the running service has OCR permission. The OCR language is inferred from embedded track metadata, a VobSub IDX language section, or a language suffix in a sidecar filename. The image OCR language packs are installed in the container. If the language cannot be identified, `CONVERTER_OCR_LANGUAGE` supplies the fallback and defaults to `eng`; OCR does not detect a language by recognizing the subtitle image itself. Use filenames such as `Example.fr.sup` when an image sidecar has no other language metadata. Language metadata and available OCR language packs affect image conversion; codecs or formats that the tools cannot read can still fail.

OCR takes longer than exporting text tracks and can misread names, punctuation, or stylized text. Review OCR output before relying on it. SRT cannot preserve ASS styling, positioning, or karaoke effects. Subtitle Edit warnings and conversion failures appear in the service logs; the watcher keeps source files and existing SRTs. It waits until an input's modification age reaches the configured minimum and records fingerprints in its persistent state volume. If the state volume is removed, inputs are scanned and processed again; existing output SRTs remain protected.

The converter stages each output beside its source, removes empty timed cues in that staging area, validates the completed SRT, then publishes it without replacing a destination. The source and any existing SRT remain untouched if conversion or validation fails. On Linux, publication uses a same-filesystem hard link; filesystems that do not support that operation will fail the conversion and leave the source untouched. A source that changes during conversion is not published.

## Configuration

The Compose service lets you change the four host-side library folders through `.env`; the container destinations are fixed by the standard Compose file and define the available media mounts:

| Variable | Host folder mounted at | Default |
|---|---|---|
| `CONVERTER_MOVIES_DIR` | `/movies` | `D:/film` |
| `CONVERTER_MOVIES_C_DIR` | `/movies-C` | `C:/film` |
| `CONVERTER_TV_DIR` | `/tv` | `D:/tv show` |
| `CONVERTER_TV_C_DIR` | `/tv-C` | `C:/tv show` |
| `CONVERTER_SCAN_INTERVAL` | Initial library-discovery interval, in seconds; values below six hours are clamped | `21600` |
| `CONVERTER_CPU_LIMIT` | Container CPU ceiling | `1` |
| `CONVERTER_GPU_MEMORY_LIMIT` | Optional GPU image container RAM ceiling; does not cap GPU VRAM | `1536m` |
| `CONVERTER_MIN_AGE` | Minimum file modification age before conversion, in seconds | `120` |
| `CONVERTER_TIMEOUT` | Maximum runtime for an individual conversion operation, in seconds | `7200` |
| `CONVERTER_RETRY_INTERVAL` | Delay before retrying an unchanged failed manual conversion, in seconds | `3600` |
| `CONVERTER_MEMORY_LIMIT` | Container memory ceiling; 512 MiB is a text-only option | `1536m` |
| `CONVERTER_ENABLE_OCR` | Permit image OCR; CPU Compose reads this setting, while CUDA Compose enables it | `false` in CPU Compose |
| `CONVERTER_ENABLE_GPU` | CUDA runtime permission for direct Docker runs; CPU Compose forces `false`, CUDA Compose sets `true`. Environment values cannot make the CPU edition GPU-capable | `false` in CPU Compose |
| `CONVERTER_OCR_LANGUAGE` | Tesseract fallback language for unknown image-subtitle language | `eng` |
| `CONVERTER_BIND_ADDRESS` | Host interface for the dashboard publication | `127.0.0.1` |
| `CONVERTER_ALLOWED_HOSTS` | Comma-separated `Host` header allowlist; preserve the defaults and add your browser hostname if needed | `localhost,127.0.0.1,::1,subtitle-converter` |
| `CONVERTER_UI_PASSWORD` | Dashboard password (12–256 printable characters); blank generates and saves one in `/data/ui-password` | Blank |
| `CONVERTER_WEBHOOK_TOKEN` | Shared Sonarr/Radarr webhook token; blank disables the hook endpoints | Blank |
| `BAZARR_INTERNAL_URL` | Bazarr URL reachable from the converter on `homelab` | `http://bazarr:6767` |
| `BAZARR_API_KEY` | Bazarr API key; keep it private in `.env` | Blank |

The four `CONVERTER_*_DIR` paths are required Compose inputs. `CONVERTER_UI_PASSWORD` may stay blank (Subforge generates and persists a private password), and `CONVERTER_ALLOWED_HOSTS` may stay at its local/service-name default. For the CPU edition, `CONVERTER_ENABLE_OCR` is optional and defaults to false. The CUDA Compose override enables both OCR and GPU permission and supplies the NVIDIA visibility/capability settings; do not set worker-count environment variables, because worker allocation is saved through **Settings → Workers** in `/data/workers.conf`.

For example, to use `/srv/media/movies` as the first movie library on Linux, set `CONVERTER_MOVIES_DIR=/srv/media/movies` in `.env`; the container will still see it at `/movies`. On Windows, use a drive path available to Docker Desktop, such as the example `D:/film`. The mounted host folders must be writable by container UID/GID 1000 so the converter can create SRT sidecars. Keep them aligned with Bazarr and media-manager access. In Subforge, **Settings → Media Management** lets you select which mounted folders or subfolders to scan; add, browse, scan, or remove roots there. This selection is saved immediately and applies to future scans. Folder browsing is restricted to the configured mounts. Changing a `CONVERTER_*_DIR` changes the host folder behind its corresponding mount; changing container-side destinations instead requires updating the Compose mount and matching `subforge --root` arguments. The `subtitle_converter_data` named volume mounted at `/data` separately persists the generated dashboard password, settings, selected scan roots, job history, and scan state across rebuilds. `.env.example` keeps API-key entries blank; store real service keys and any chosen dashboard or webhook passwords only in the ignored `.env`.

## Status and resource readings

**Settings → Status** has separate About, Live resources, and Execution limits panels. While this section is visible in a visible browser tab, resource readings refresh every three seconds and the displayed uptime advances locally once per second. Both timers stop when you leave Status or hide the tab. The existing dashboard status polling remains separate: it polls every three seconds while work is active, every 15 seconds while idle, and every 60 seconds in a hidden tab. `/api/system` uses the dashboard's existing authentication behavior. Execution limits now reflect the effective worker allocation, while **Settings → Workers** shows the requested allocation, deployment ceilings, measured CPU/RAM headroom, and active files.

The readings are observations, not scheduling controls. On supported Linux Docker deployments, CPU and memory usage come from the container cgroup and include conversion child processes; Rust process RSS and thread count cover only the Subforge server process. CPU usage is shown as a percentage of the effective CPU allowance, where 100% means the full allowance (the panel also shows CPU equivalents used). The allowance reflects the visible cgroup quota and process CPU affinity. It may not include ancestor limits outside the container's visible cgroup namespace. The first CPU sample, a counter reset, or an unsupported source can leave a reading unavailable; the UI reports that state instead of showing a fabricated zero. Native Windows kernel resource readings are unavailable. Linux process RSS and thread readings may still be available when cgroup metrics are unavailable, such as a shared-host Linux setup whose cgroup namespace does not expose the process container root.

Container RAM is current cgroup usage against its reported memory ceiling, while process RAM is the Rust server's resident set only. The metadata cache shows its entry count and accounted payload bytes against the 5,000-entry and 32 MiB limits. Those byte counts are approximate serialized-payload bookkeeping, not actual heap usage or an additional memory reservation. The separate Library inventory remains limited to 2,000 inspected files; this inventory cap is unrelated to the metadata cache entry limit.

### Worker allocation

**Settings → Workers** reads and updates `/data/workers.conf`. The application generates this private JSON configuration file atomically when it is missing. Worker counts and ceilings are read from this file, not environment variables. An existing `workers.json` allocation is migrated when present. The initial configuration is:

```json
{
  "cpu_workers": 1,
  "ocr_workers": 1,
  "gpu_workers": 0,
  "paused": false,
  "max_cpu_workers": 4,
  "max_ocr_workers": 1,
  "max_gpu_workers": 1,
  "gpu_min_free_mib": 2048
}
```

The UI saves allocation changes immediately to this file; they apply to newly dispatched files. Restart Subforge after manually editing the file's ceilings or GPU free-memory threshold. `GET` and `PUT /api/workers` expose the authenticated allocation for API clients; strict validation rejects unknown fields or counts outside the configuration and deployment limits. CPU workers handle independent files; a CPU OCR slot shares the CPU pool, while a GPU OCR slot is separate. Each external tool remains limited to one CPU thread. One top-level job runs at a time, while files inside it can run concurrently when their possible output names do not conflict. Files with equal or dot-suffixed stems in the same folder are serialized to protect shared output names; distinct episode files can run concurrently.

Changing allocation affects newly dispatched files; active files finish with the allocation they started under. **Pause new file work** stops dispatch and lets active workers finish. This is separate from pausing automatic schedules in **Settings → Tasks**; it does not change discovery or OCR schedule intervals, and queued work waits for the worker pool to resume. OCR slots are effective only when image OCR permission is enabled. Requested worker counts are bounded by workers.conf ceilings, CPU quota/affinity, and soft memory eligibility estimates. The application estimates 128 MiB for its server process, 256 MiB per CPU file worker, and 1 GiB per CPU or GPU OCR file worker. These estimates gate concurrent dispatch when a memory ceiling is visible; they are not guaranteed peaks or reserved memory. Under a tight RAM ceiling, OCR workers wait while CPU text work can continue.

The configuration's `max_cpu_workers` defaults to 4 and is hard-capped at 8. `max_ocr_workers` defaults to 1 and is hard-capped at 2, with CPU OCR slots always limited by the CPU allocation. `max_gpu_workers` is hard-capped at one and is effective only with the explicitly granted GPU runtime and device. `CONVERTER_CPU_LIMIT` separately sets Docker's CPU ceiling. A value of `4` allows the container to use up to four CPUs; it does not request four jobs by itself. Increase the allocation in **Settings → Workers** after the container has enough CPU and RAM capacity. Changing Docker's `CONVERTER_CPU_LIMIT` requires recreating the service; `docker compose restart` alone retains the existing container configuration. Use this generic command from the repository root on PowerShell, Bash, or Zsh:

```shell
docker compose up -d --force-recreate --no-deps subtitle-converter
```

This recreates only `subtitle-converter`, without rebuilding its image or starting dependencies.

### Optional NVIDIA GPU OCR

The default Compose service and CPU image expose no GPU devices. The optional path uses a separate `linux/amd64` image built with `Dockerfile.cuda`, a `cuda` Rust feature, and the PaddleOCR helper with packaged language-specific mobile OCR models. The CUDA-tools stage pins Python 3.12, PaddleOCR/PaddleX 3.7.0, and Paddle GPU 3.2.2 for CUDA 11.8, separately from the Rust CUDA build stage. The GPU stack has 80 full-hash-locked dependencies plus a separate setuptools 84.0.0 runtime-packaging lock (81 pins across two files); the final image passes `pip check`, and 15 model archives are checksum-verified at build time. A normal build uses the pinned upstream wheel and model sources and needs network access. Optional BuildKit wheel and model-archive caches can avoid downloading matching artifacts without adding cache payload to the runtime image. From the repository root, build the CPU edition first. To reuse the Paddle GPU wheel, place `paddlepaddle_gpu-3.2.2-cp312-cp312-linux_x86_64.whl` in `output/gpu-cache/wheels`; place any desired model archives in `output/gpu-cache/models`.

```shell
docker build --platform linux/amd64 --file subtitle-converter/Dockerfile.cuda --build-context gpu-wheel-cache=output/gpu-cache/wheels --build-context gpu-model-cache=output/gpu-cache/models --tag homelab-subtitle-converter:0.4.0-cuda subtitle-converter
```

The wheel and model cache contexts are optional; the build works with empty contexts and downloads from the pinned sources. It verifies cached files against their locked hashes, caps model archives at 128 MiB, and mounts both caches read-only at build time. The model layer runs before the helper is copied, so helper-only edits do not force model downloads again.

The running OCR helper uses the packaged models and does not download models on first startup. CUDA execution is checked explicitly, including a GPU tensor and available memory. If CUDA cannot be used or the selected model language is unsupported, GPU OCR fails; it does not silently fall back to CPU OCR.

This configuration is provided for compatible NVIDIA-enabled Docker hosts on Linux/amd64. Windows requires Docker Desktop's WSL 2 backend with NVIDIA GPU support enabled; macOS GPUs do not provide CUDA. These are platform requirements from the image and runtime configuration; this repository's configuration alone does not establish that a particular host, driver, or Docker setup works. Check [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html) and [Docker Desktop GPU support](https://docs.docker.com/desktop/features/gpu/) for host prerequisites.

The CPU and CUDA images are built separately; Compose never builds or pulls these local tags. Build CPU first because the CUDA Dockerfile reuses its runtime base, then build CUDA:

```shell
docker build --target final --tag homelab-subtitle-converter:0.4.0-cpu subtitle-converter
docker build --platform linux/amd64 --file subtitle-converter/Dockerfile.cuda --tag homelab-subtitle-converter:0.4.0-cuda subtitle-converter
```

The same commands work in Windows PowerShell:

```powershell
docker build --target final --tag homelab-subtitle-converter:0.4.0-cpu subtitle-converter
docker build --platform linux/amd64 --file subtitle-converter/Dockerfile.cuda --tag homelab-subtitle-converter:0.4.0-cuda subtitle-converter
```

Create the ignored root `.env` from `.env.example`, set the four host media directories, and start the CUDA edition with its override:

```shell
docker compose -f docker-compose.yml -f compose/subtitle-converter.cuda.yml up -d --no-deps subtitle-converter
```

The override grants only NVIDIA device `0`, with the `compute` and `utility` driver capabilities, and sets `CONVERTER_ENABLE_OCR=true`, `CONVERTER_ENABLE_GPU=true`, `NVIDIA_VISIBLE_DEVICES=0`, and `NVIDIA_DRIVER_CAPABILITIES=compute,utility`. It does not grant every GPU or the Docker socket. For direct `docker run`, set those four environment values and separately grant device access (for example `--gpus device=0`); environment variables alone do not expose a GPU. The CUDA-edition dashboard, API, sidebar, and header identify the compiled edition, while **Settings → Workers** reports whether the runtime and device are eligible. The saved GPU worker allocation remains zero until you set **GPU OCR slots** to one. The default free-memory threshold in workers.conf is 2048 MiB. With the GPU worker allocation left at zero, Subforge starts no GPU OCR work and leaves device memory available to other GPU applications such as LM Studio. The threshold is checked before work, but Docker does not reserve or cap GPU VRAM. Keep the GPU worker count at one when sharing the device. The override configures a `1536m` container RAM ceiling by default; this is system memory, separate from device VRAM. The small-fixture validation result and its hardware limits are documented in [Development checks](#development-checks).

To return to the CPU image, recreate the service from the ordinary Compose entrypoint without the CUDA override. On startup, Subforge clamps the saved worker allocation to the compiled edition's permissions and deployment limits, then persists the adjusted values, so GPU slots return to zero:

```shell
docker compose up -d --force-recreate --no-deps subtitle-converter
```

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
docker compose exec subtitle-converter subforge integration-check bazarr
```

Replace `bazarr` with `sonarr`, `radarr`, or `jellyfin` to check another integration. The diagnostic uses the converter's configured credentials, prints only the provider, connection result, and—for Jellyfin—library/selection counts. It does not print server addresses, library names, API keys, or raw server responses. A nonzero exit status means the configured connection check failed; use the dashboard's safe diagnostic and service logs for next steps. This check verifies connectivity only and does not change server settings or queue scans.

## Memory use and OCR

Compose and `.env.example` give Subforge a `1536m` memory ceiling by default while keeping image OCR disabled. To enable image-track extraction, set `CONVERTER_ENABLE_OCR=true` in the private `.env` and recreate the service; the default ceiling already matches the OCR configuration. When OCR remains disabled, a text-only deployment can set `CONVERTER_MEMORY_LIMIT=512m`, but TTML and SBV use Subtitle Edit even without OCR, so validate the limit against your subtitle formats. The General settings screen reports whether OCR is enabled; it does not change the environment-controlled permission. Every media-tool worker uses one native thread. File-level parallelism is bounded by the saved worker allocation, Docker CPU ceiling and affinity, and the soft per-worker memory estimates described in [Worker allocation](#worker-allocation). These estimates are admission guidance rather than reserved host memory or guaranteed peak usage; Docker uses less than its ceiling when the workload needs less.

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

The native server enables persisted scheduled tasks by default; pass `--no-auto-scan` to disable automatic scheduling. Set environment variables such as `CONVERTER_ENABLE_OCR`, `CONVERTER_OCR_LANGUAGE`, and the media-server integration URLs/keys in the shell environment before running. Keep API keys private.

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

The GPU adapter and model-archive unit tests use Python's standard library and fake CUDA modules; CI runs them in a pinned container with the GPU source mounted read-only and networking disabled; CI also audits the adapter source and both Python lock files with pip-audit, and checks the external Paddle GPU wheel version separately through OSV. The optional hardware smoke test in `tests/gpu_smoke.py` must run from the GPU image with `/tests` mounted read-only, network access disabled, and NVIDIA device 0 explicitly granted. The hardware smoke test passed on an NVIDIA RTX 3060 with CUDA and one explicitly exposed GPU, using 200 distinct synthetic SUP subtitle cues. The read-only, network-disabled container completed in 9.32 seconds including startup and the cached repeat; peak container memory was 1,097,826,304 bytes (about 1,047 MiB), the Rust server high-water RSS was 6.3 MiB for this tiny fixture, the GPU worker was observed, the source remained unchanged, and the repeat used cached OCR output. This validates that fixture on that GPU/driver setup only; it does not predict full-library memory peaks or conversion speed, or establish compatibility with other NVIDIA devices and drivers.
After building the CUDA image, run this hardware smoke test from a Linux Bash or WSL 2 shell at the repository root. It exposes only NVIDIA device 0 and mounts the test fixtures read-only:

```shell
docker run --rm --gpus device=0 --network none --read-only \
  --cap-drop ALL --security-opt no-new-privileges --memory 1536m --cpus 2 \
  --tmpfs /tmp:rw,nosuid,nodev,size=512m \
  --mount type=bind,source=./subtitle-converter/tests,target=/tests,readonly \
  --entrypoint python3.12 homelab-subtitle-converter:0.4.0-cuda \
  /tests/gpu_smoke.py --cues 200
```

The legacy Python implementation and its fixture tests remain in the repository as reference material while Rust parity checks continue; they are not part of the runtime image. Native Rust development requires the tools listed above and permission to create temporary files next to each input so completed SRTs can be published without overwriting existing files.
