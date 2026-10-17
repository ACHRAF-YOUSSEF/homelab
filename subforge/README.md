# Subforge — Automatic Subtitle Conversion

Subforge is the subtitle-conversion dashboard, with Compose service and project directory both named `subforge`. It is a Rust watcher and local web dashboard that creates UTF-8 SubRip (`.srt`) sidecars from embedded subtitle tracks and supported subtitle files. FFmpeg handles common text tracks; TTML and SBV sidecars use the official [Subtitle Edit command-line converter](https://github.com/SubtitleEdit/subtitleedit/releases) even when OCR is disabled. Image-based subtitle tracks use a native Rust `ocr-rs` 2.4.1 helper backed by MNN 3.4.1 when OCR is enabled. Its verified offline models explicitly support English (`en`), French (`fr`, using the Latin model family), and Arabic (`ar`), alongside additional language families. Both CPU and CUDA production and test images contain no Python or PaddlePaddle runtime; the CUDA edition builds the same helper with MNN CUDA support. MNN is native C++ code and the app still invokes external tools such as FFmpeg and Subtitle Edit, so the complete application is not Rust-only.

### Upgrade from the former service name

The service used to be named `subtitle-converter`. Docker does not change labels on an existing container when a Compose service is renamed. Build the new local image first and back up the retained data. Inspect the exact old container and confirm its Compose project, service label, and mounted volume before stopping it. Do not remove similarly named containers such as `compose-subtitle-converter-1` unless inspection confirms they belong to this installation. Run Compose from the repository root with the same project name as the old container; `homelab` is the standard example, but if inspection shows a custom prior project, use that exact project name in every migration Compose command. This retains the existing physical volume through the unchanged Compose key `subtitle_converter_data`; inspect the mount because another prior project name may produce a different physical name.

```shell
docker inspect subtitle-converter --format '{{.Name}} {{index .Config.Labels "com.docker.compose.project"}} {{index .Config.Labels "com.docker.compose.service"}} {{range .Mounts}}{{.Destination}} {{.Type}} {{.Name}}; {{end}}'
docker volume ls
docker stop subtitle-converter
docker rm subtitle-converter
docker compose --project-name homelab up -d --no-deps subforge
```

For CUDA, keep the same project name and use the `-f compose/subforge.cuda.yml` override on the start command:

```shell
docker compose --project-name homelab -f docker-compose.yml -f compose/subforge.cuda.yml up -d --no-deps subforge
```

After removal, verify the inspected volume still exists; the command removes only the container object. Do not use `down -v` or `--remove-orphans` for this rename. The persistent volume retains Subforge's password, settings, jobs, and scan state. Keep the existing `CONVERTER_*` variable names. If `.env` explicitly sets `GLANCE_SUBFORGE_INTERNAL_URL` or `CONVERTER_ALLOWED_HOSTS` to the former service hostname, update that value to `subforge` only where the service name is the intended host.

## Start the service

Compose requires the ignored root `.env` for interpolation. Create it from `.env.example` if it does not exist, then set the four `CONVERTER_*_DIR` values to host folders used for your movie and TV libraries. The sample paths match the current Compose layout; replace them with paths on your machine. On Windows with Docker Desktop, use drive paths shared with Docker Desktop. On macOS and Linux, use absolute host paths such as `/Volumes/media/movies` or `/srv/media/movies`. The dashboard binds to loopback by default; `CONVERTER_ALLOWED_HOSTS` defaults to the local names and service name. Dashboard Basic Authentication is enabled by default; `CONVERTER_UI_PASSWORD` may remain blank to generate a persistent password. Set `CONVERTER_AUTH_ENABLED=false` in the ignored root `.env` to disable dashboard login. Media-service keys are optional; keep any real keys in the ignored `.env`. Direct `docker run` does not read Compose's `.env`; supply equivalent runtime settings and all four media bind mounts plus persistent `/data` yourself.

Build the selected edition locally before starting it. Compose only starts a prebuilt tag: it neither builds nor pulls Subforge, and startup fails if the selected local tag is missing.

```shell
docker build --target final --tag homelab-subforge:0.4.4-cpu subforge
```

PowerShell uses the same local build command:

```powershell
docker build --target final --tag homelab-subforge:0.4.4-cpu subforge
```

Then start the prebuilt CPU image from the repository root:

```shell
docker compose up -d --no-deps subforge
```

Open the responsive Subforge dashboard at `http://localhost:8100`. Routes are bookmarkable: reloading the page and using browser back/forward preserves the selected page and Settings section. Settings use full-width tabs for Preferences, Media folders, Tasks, System, and media-server Connections. Overview combines library totals with actionable counts, pairs Quick conversion and local worker allocation, and shows Current work and active file workers while work is active. Status counters render from `/api/status` without waiting for the separate detailed inventory request. Worker nodes and the operation log are inside expandable **Worker nodes and diagnostics**. Resource and worker information refresh while Overview is visible; the old `#/settings/workers` bookmark redirects to Overview. The shell fills the dynamic viewport; the top bar stays in place while the workspace and sidebar scroll independently. The top-bar chevron remains visible when expanding or collapsing desktop navigation. On narrow screens the navigation opens as a drawer and inventory rows become stacked file cards. Native select menus retain keyboard behavior and use the same dark styling as other controls. Dashboard HTTP Basic Authentication defaults to enabled with username `admin`; `CONVERTER_AUTH_ENABLED=false` disables only this login. When enabled, a blank `CONVERTER_UI_PASSWORD` causes the service to create a random password once and store it in `/data/ui-password` with mode `0600`; the password is not written to logs. Disabling authentication does not load, generate, or validate that password, and preserves the saved file so it is available if authentication is enabled again. Retrieve the generated password from PowerShell, Bash, or Zsh with:

```shell
docker compose exec subforge cat /data/ui-password
```

Alternatively, set `CONVERTER_UI_PASSWORD` in the ignored root `.env` to a private password of at least 12 characters. To opt out, add `CONVERTER_AUTH_ENABLED=false` to the ignored root `.env`; this is environment-only configuration. With login disabled, the dashboard's media inventory and controls are available to anyone who can reach the service, so keep it on trusted localhost or a private network, or place it behind an authenticating reverse proxy. The host allowlist and same-origin/CSRF checks still apply. This setting does not disable the independently authenticated webhook, widget, or remote-worker endpoints; their dedicated tokens remain required when those features are used. After changing the flag, recreate Subforge using the same Compose files as the selected edition; rebuild that local CPU or CUDA image first when deploying new code. The named `subtitle_converter_data` volume preserves the generated password, jobs, settings, preferences, worker allocation, and scan state across container rebuilds. The Rust runtime reads existing state/settings/preferences/history files and the password, preserving compatible data when the image is upgraded; Jellyfin connection details remain managed through environment variables.

The library inventory is saved separately in `/data/inventory.json`, so it survives page reloads, container restarts, and job-history rotation. Search, the Issues/Has tracks/Pending OCR/Failed/Ready/Metadata and media/root filters, page size and selected page survive reloads in the same browser tab. An inspection keeps the previous inventory visible until it finishes; cancelling it keeps that saved snapshot. Startup does not automatically build an inventory or convert files. Use **Library → Inspect media** to create or refresh the snapshot, which is limited to 2,000 files and stored with private permissions and bounded metadata; status polling sends only its summary, and the browser fetches the rows when the snapshot changes. The Overview pipeline counters use cached processing state and update after job-state merges. They render as soon as `/api/status` returns, without waiting for `/api/inventory`; polling does not trigger filesystem walks. An explicit or scheduled inventory job may reconcile a saved pending or failed processing record only when verified local coverage matches the same source file fingerprint and exact track scope. This changes current counters while preserving saved errors, job history, and published-output receipts. Unknown, mismatched, or still-uncovered records remain pending or failed. Each job request is limited to 256 input paths, and an individual track selection is limited to 64 IDs.

### Dashboard workflow

The Overview Library status summarizes inspected media items and generated SRT files alongside actionable failed, pending OCR, and unavailable-folder counts. It does not replace the searchable per-file Library inventory. When work is active, Overview shows its current progress and active file workers; Quick conversion and local worker allocation remain together on the Overview. Remote worker nodes and the operation log are available under expandable **Worker nodes and diagnostics**. **Inspect media** refreshes the inventory without converting files; when Bazarr is enabled, this action is labeled **Sync Bazarr inventory**. **Convert subtitles** opens a scope review before adding the job. A manual conversion requests OCR for missing image subtitles only when the service grants OCR permission; it uses available CPU or GPU slots. Scheduled discovery stays text-only, and the separate OCR task processes eligible image subtitles. The separate Glance processed-file total counts inputs with a tracked completed conversion pass, not generated SRTs; completed text passes remain counted if later OCR fails.

Open an inventory row's **Details** drawer to inspect and select individual embedded or sidecar tracks. The drawer uses unambiguous IDs for embedded subtitle streams and VobSub tracks; unsupported or ambiguous IDs stay disabled until reinspection. A lone sidecar source can be selected without a stream ID. Image tracks are selectable only when OCR is permitted by the service environment. The drawer shows the OCR permission state and sends the selected IDs with the extraction request. Activity filters separate job status from work type and default to subtitle work; choose **All operations** or **Maintenance** to include health, cleanup, and backup jobs. Failed files show the latest recorded cause in the inventory. The Activity job details drawer groups conversion failures and integration errors by source/provider with bounded examples and links to the relevant connection settings. Job cards offer **Retry failed** for recorded failures, bounded to the first 64 paths when a job has more; a single-path retry also retains its selected track IDs. Automatic discovery and scheduled OCR defer unchanged failures unless a manual retry is requested. Scoped failure records follow the current file fingerprint; a successful whole-file pass clears only selections it fully covers, leaving unresolved track-specific failures intact.

Default conversion languages are saved in Preferences. Quick conversion has a separate language selection for that job, so changing it does not silently change the saved default. The review step repeats the selected languages and folders before the job starts.

### Bazarr-authoritative inventory and conversion

When Bazarr is enabled, its language profiles define the wanted language, forced, and hearing-impaired (HI/SDH) subtitle variants for each movie and episode. **Sync Bazarr inventory** reads the language profiles, all paged movie and series records, and episode records in bounded series-ID batches. For each wanted variant, Subforge checks the sibling media files: only a non-empty, structurally valid SRT with the matching language, forced, and HI flags counts as coverage. Bazarr's `missing_subtitles` value by itself does not establish that an SRT is present or valid. Clearing the URL or API key keeps Bazarr selected as the inventory authority and blocks automatic conversion until the connection is restored. Existing embedded tracks or supported sidecars are probed as potential sources for variants that remain missing.

Automatic conversion only uses sources with known language and variant metadata that match a missing profile target. Bazarr can supply explicit metadata for a sidecar such as `Movie.ass` even when its filename has no language; conflicting filename metadata is rejected. Regional aliases such as `pb`/`pob`, `zht`, and `spl` remain distinct from generic Portuguese, Chinese, and Spanish, and the Hindi `.hi.srt` language suffix remains distinct from the hearing-impaired `.en.hi.srt` flag. Untagged tracks are left for explicit inspection and selection. A legacy output such as `movie.track3.und.srt` is not evidence that a wanted language is covered, and Subforge does not remove old sidecars. To handle an untagged track or intentionally create another track for a variant that already has an SRT, inspect the file, select the track, and use **Allow another selected track for a language that already has SRT** in the track drawer. This is a manual override; it does not widen scheduled or automatic conversion.

The Bazarr inventory uses the configured Bazarr base URL and reads `/api/system/languages/profiles`, paged `/api/movies` and `/api/series`, and `/api/episodes` for batches of series IDs. On incomplete pages, unsupported profile data, request failure, cancellation, or exceeded bounds, Subforge keeps the prior snapshot marked stale or unavailable and blocks automatic conversion until a successful sync. A fresh sync can succeed while Bazarr is still indexing newly published SRTs; this `refresh_pending` state is a warning, not a conversion lock. The warning banner is hidden when authority returns to `ready`. The conversion button stays available in `refresh_pending`, and Subforge checks local SRT coverage before choosing targets. Stale or unavailable inventory and a definite reindex failure still block conversion. If verification exceeds tracking limits, Subforge retains the pending history confirmation rather than treating it as cleared; this does not by itself permanently block conversion. The inventory is bounded to 20,000 media items; it is not an unbounded full-library crawl. Conversion candidates and metadata probes have additional limits, so very large libraries may require a narrower folder scope.

Published SRT paths are durably aggregated and scanned after the job queue and all active jobs drain; the details below describe the bounded publication queue. Lightweight requests keep an 8-second timeout; the exact scoped scan request can wait up to 120 seconds within a 300-second notification batch, and inventory fetches allow 90 seconds. A timeout, malformed success response, or transport loss after a scan PATCH is treated as uncertain: Subforge does not repeat the scan automatically. A fresh inventory read that does not yet contain every published SRT leaves indexing confirmation pending and displays a warning; it does not disable conversion while the fresh inventory remains usable. Raw socket timeout errors are classified as timeouts; explicit HTTP errors and connection-refused responses remain definite failures and set `reindex_failed`, which blocks conversion until a successful sync. Use **Check Bazarr indexing** for that read-only check; it does not send another scan. Pending verification survives restart and clears after a later inventory sync sees all tracked outputs. If output tracking limits prevent complete confirmation, the history remains marked verification-limited and cannot be declared fully verified. A whole-series scan can take longer than the connection test. Do not re-extract SRTs that already exist to resolve indexing delay. With Bazarr disabled, Subforge uses its local filesystem inventory and conversion behavior.

### Job progress and liveness

Overview, Activity rows, and the Activity details drawer show elapsed time and file-level progress when available. Activity also shows the current phase (such as preparing, discovering, syncing Bazarr, waiting for a worker, OCR, extraction, or SRT validation) and explicit worker wait reasons. A completed-file ETA is approximate: it appears only after at least one actual file completes, uses completed-file throughput, and excludes initial files skipped because they were already covered. A one-file job therefore remains at 0% with no ETA until that file completes; OCR cue/frame activity is not a reliable whole-file percentage.

During file dispatch and inventory-lock waits, Subforge sends a coordinator heartbeat every 15 seconds. Timeout thresholds follow bounded work phases: 120 seconds while syncing Bazarr, 330 seconds while notifying Bazarr, 300 seconds while discovering files, and 60 seconds for ordinary work. Five minutes without a change in reported progress is labeled **Possibly stalled**; a heartbeat gap that reaches the phase threshold is labeled **Unresponsive**. Inventory-lock waits report a waiting reason and remain cancel-aware. These are diagnostic states only: Subforge does not automatically fail or cancel a job based on them. A heartbeat confirms the coordinator is alive, not that the underlying media tool is advancing. Quiet OCR can still be labeled possibly stalled while the OCR process is working, because cue/frame activity does not reliably indicate whole-file progress. Queued jobs have no elapsed runtime until they start.

Integration status dots summarize the latest observed connection test or recorded integration request. They are not live health checks, and Subforge does not poll each provider in the background. Jellyfin library selectors display library names with their IDs as helper text; up to 100 loaded names and IDs are cached in the current tab session so selectors can be restored without another lookup. The Settings → System page includes live CPU, RAM, worker-capacity, and cache-usage bars; unavailable measurements remain unavailable rather than being estimated.

The Glance widget reports failures from the latest conversion scan, rather than adding repeated failures from old jobs. Historical activity remains available for diagnosis. The integration settings explain each provider's automation and show its latest recorded request; successful connection tests alone do not register Sonarr or Radarr import webhooks.

Unix media tools run with a fixed UTF-8 locale so non-ASCII Matroska titles cannot truncate their JSON output. Inspection falls back to FFprobe when MKVToolNix returns unusable metadata, and mismatched native track tables are discarded rather than paired with the wrong subtitle streams. SeConv output is parsed from its bounded JSON report even when diagnostic warnings surround the JSON. The converter canonicalizes supported timestamp separators and hour padding, repairs repeated blank-line breaks inside FFmpeg-generated cue text, removes empty timed cues, then strictly validates staged SRT before publication. Invalid cue numbers or timing still fail safely. Conversion failures identify the track or stream, language, cue number and reason without logging subtitle text. Successful unchanged conversions stay cached. Automatic discovery and scheduled OCR defer unchanged failures. Job cards include **Retry failed** for recorded failed paths; the **Include unchanged failed tracks in the next manual OCR run** option explicitly retries recorded OCR failures. **Convert selected** may also include OCR for the selected file when deployment permission enables it. Run **Inspect media** again to replace a snapshot containing older inspection errors.

### Troubleshoot startup

If authentication is enabled and the service exits during startup with an invalid `CONVERTER_UI_PASSWORD`, set it privately in the ignored root `.env` to a strong printable password of 12–256 characters, or leave it blank to use the persistent generated password. When authentication is disabled, the password is not loaded or validated. Recreate only this service after changing the environment:

```shell
docker compose up -d --no-deps subforge
```

For a CUDA deployment, keep `-f compose/subforge.cuda.yml` in the command so troubleshooting does not switch the service to the CPU image.

When using the generated password, retrieve it with the command above. Startup validation reports the setting and phase without printing its value; never paste credentials into logs or shared documentation.

Compose publishes container port `8100` on host port `8100`, bound to `127.0.0.1` by default through `CONVERTER_BIND_ADDRESS`. The service joins `homelab` so the dashboard can reach configured Sonarr, Radarr, Bazarr, and Jellyfin servers and receive Sonarr/Radarr webhooks. Add the hostname used in your browser to `CONVERTER_ALLOWED_HOSTS` when you use a non-local URL; retain the default entries. The host allowlist complements the bind setting and firewall. When dashboard authentication is disabled, expose it only to trusted clients or protect it with an authenticating reverse proxy.

The breadcrumb, refresh button, and navigation collapse control stay in the top bar while scrolling. Desktop navigation collapse is remembered in that browser. On mobile, the menu opens a drawer; Escape closes it and focus returns to the menu button.

Docker needs network access while building the pinned Rust application and OCR tool. The running service uses the `homelab` network for its web UI and configured integrations and has no Docker socket. The default CPU image does not reserve GPU devices. The process runs as UID/GID 1000 with a read-only container root, dropped capabilities, and `no-new-privileges`; it writes generated SRTs into the mounted libraries and private state into `/data`. The default CPU runtime image contains the Rust executable and media tools, not Python. The bounded HTTP server has four request workers and a 16-connection queue; the job queue is limited to 32. File-level conversion runs within the bounded allocation described below.

**Library → Inspect media** lists files and subtitle tracks without converting media; open a file card or row and use **Convert selected** for chosen tracks. Search matches filenames and full paths case-insensitively and combines with the root and status/media filters. Library and Activity keep their headings, filters, and pagination visible while only table rows scroll. The default **Auto** row count fits responsive rows to the available table height; manual choices are 25, 50, or 100 Library rows and 10, 25, or 50 Activity jobs. Page and filter choices persist in the current browser tab; changing filters resets to page one. Inspections return at most 2,000 files, and the Library page identifies a capped result so you can inspect a smaller root. Activity filters by status and work type, starts with subtitle jobs, and can include maintenance work. Its paginated table shows job, status, worker, result, updated time, and actions. Open **Details** for full errors, pending indexing notifications, retry controls, and the retained executor list; Overview instead shows the current job only while work is active. You can cancel queued or running jobs and follow a provider error to its connection settings. Preferences stores default languages separately from the current quick-conversion language choice. OCR permission remains environment controlled; Overview shows requested and effective CPU/GPU concurrency, resource admission, active file workers, and connected worker nodes.

### Scheduled tasks

**Settings → Tasks** exposes the fixed discovery, OCR, inventory, health, cleanup, and backup tasks. It shows each task's enabled state, interval, next run, last run, status, and result. Changes and schedule-pause state persist in `/data/tasks.json`. There is no special startup conversion run; fresh schedules set their first run relative to service startup, while saved schedules keep their persisted next-run time. The scheduler keeps queued jobs ordered. Explicit selected-file scan, extraction, and OCR jobs can run concurrently, up to 32 active jobs and the shared CPU/GPU worker-admission limits. Full-library scans and inventory or maintenance jobs remain exclusive to bound memory use. `/api/status` exposes concurrent work in `active_jobs`; `running` remains as the oldest-job compatibility field. Scheduled task pause and worker-pool pause are separate settings.

| Task | Default interval | Minimum interval | Default state and behavior |
|---|---:|---:|---|
| Library discovery | 6 hours | 6 hours | Enabled; finds supported files and performs automatic text conversion. |
| Subtitle OCR | 1 day | 6 hours | Enabled only when `CONVERTER_ENABLE_OCR=true`; processes recorded new or changed inputs with OCR pending. It does not walk the library. |
| Media inventory | 1 day | 6 hours | Disabled; enable or run it manually to refresh the Library snapshot. |
| Health check | 6 hours | 1 hour | Enabled; checks configured mount and state-directory availability locally, with no network polling. |
| Cleanup | 1 day | 1 day | Enabled; removes stale conversion-state and metadata-cache entries only. |
| Backup | 7 days | 1 day | Enabled; writes a private backup of application state, retaining three generations. It does not include media files. |

Task intervals can be adjusted in **Settings → Tasks** subject to each task's displayed minimum; the maximum is 30 days. **Pause schedules** persists across restarts. It does not cancel a running job, and manual Run buttons remain available while paused. On first startup, `CONVERTER_SCAN_INTERVAL` sets the discovery interval; values below 21,600 seconds are clamped to six hours, and Compose defaults it to `21600`. After that, the saved interval in `tasks.json` takes precedence. Worker allocation and local-worker pause are stored separately in `/data/workers.conf`; pausing local workers stops admitting new files and lets active files finish. The metadata cache is capped at 32 MiB and 5,000 entries. Each tool process stays single-threaded; independent file workers provide bounded parallelism. Compose defaults to a 1 CPU and 1536 MiB memory ceiling; the text-only memory option is 512 MiB. Worker resource budgets are soft estimates, not reserved or guaranteed memory; see [Memory use and OCR](#memory-use-and-ocr) for details.

The runtime, CLI `--version`, API, and dashboard report the compiled package version from `Cargo.toml`; **Settings → System** and the sidebar show the version and build edition. Version is release metadata, not an editable setting or environment variable. Patch fixes advance the patch version (for example, `0.2.0` → `0.2.1` → `0.2.2`); features advance the minor version. For each revision, update `Cargo.toml` and the root package entry in `Cargo.lock`, the fixed Dockerfile image-version label, and the fixed CPU and CUDA image references in the Compose files together. The edition is compiled into each image; the CPU edition cannot be switched to CUDA by setting environment variables. Rebuilding the same revision keeps its version. Settings tabs have bookmarkable `#/settings/<section>` routes; older `#/system/tasks` and `#/system/status` bookmarks redirect to Tasks and System. See the [revision history](CHANGELOG.md) for changes in each version. The Glance processed-file total counts inputs with a tracked completed conversion pass, not generated SRT files. Completed text passes remain counted when later OCR fails, including text passes that queued image tracks without creating a text SRT. The Overview snapshot instead shows the saved inventory, pending OCR file count, and distinct SRT outputs recorded in conversion state; external SRT files are excluded, and the output count is unavailable if the bounded ledger cannot give an exact total.

The Library inventory, Activity's Jobs list, scheduled tasks, System panels, and Overview Operation log can be collapsed; Overview's Worker nodes and diagnostics is expandable. The browser remembers collapsible panel state. Activity filters by status and work type. Job failures, per-file conversion causes, integration notifications, and refresh results appear in a disclosure grouped by origin, provider, and repeated message, with a count and preview; opening it shows bounded examples and, when a provider is identified, a shortcut to its connection settings. The inventory and track drawer display the latest recorded per-file error. Jobs that finish processing files with failures are marked **Completed with errors**. The disclosure's open state and scroll position are preserved during status refreshes. Integration diagnostics give a safe HTTP or connection category without exposing API keys or raw server responses; they explain errors but do not repair an integration configuration. The Operation log writes sanitized newline-delimited JSON to container stdout; Subforge retains up to 200 events in `/api/status` and shows the latest 50 in the dashboard. Events track work phases, files, backend and track, jobs, workers, schedules, settings, and media-folder operations; dashboard GET requests and worker heartbeats do not generate log spam. Records omit subtitle text, credentials, and full media paths. New per-file phase, completion, failure, and remote-fallback retry events include the assigned node label, CPU/CPU OCR/GPU OCR resource, local/remote execution, and worker ID. Job history retains up to 64 unique node/resource/execution combinations, with an explicit truncation flag; Activity job details show the first four and let you expand the rest. These are configured node labels and assigned worker resources, not verified hostnames or continuous GPU-utilization readings (CPU preparation can surround GPU OCR). Existing history is not backfilled, so older jobs may have no executor details.

### Glance status widget

The Glance **Media** page includes a Subforge status widget. Its read-only `/api/widget` endpoint reports `current_run` for older clients and adds `active_jobs` (up to 32, oldest first) and `active_job_count` for concurrent jobs. Each job reports a safe ID, phase/status, file progress when available, elapsed time, nullable ETA, liveness, and an optional generic wait reason. It exposes no media paths or credentials and cannot start or change jobs. Glance's server-rendered fallback caches for 15 seconds; while the Media page is visible, a same-origin SSE stream replaces it with live snapshots. The gateway polls `/api/widget` every two seconds by default, only while a browser is subscribed, and stops after the last subscriber leaves. The browser retains the last successful snapshot during errors, closes the stream when the page is hidden or unloaded, and reconnects when it becomes visible again. Elapsed time and known ETA interpolate for at most 15 seconds after a snapshot. ETA still uses completed-file throughput: a single-file job has no ETA until it completes, and quiet OCR may appear possibly stalled while still working. These are diagnostic states and do not fail jobs automatically. The widget uses the copied [Subforge SVG icon](../glance/config/assets/subforge.svg) in the dashboard service list.

The widget is optional and disabled while `CONVERTER_WIDGET_TOKEN` is blank. To enable it, create a private random token between 32 and 256 characters and set the same value for `CONVERTER_WIDGET_TOKEN` in the ignored root `.env`; never put it in `.env.example`, YAML, or shared documentation. Compose passes this dedicated token to Glance, Subforge, and `glance-live`. It is separate from the dashboard password, webhook token, and media-server API keys. `SUBFORGE_PUBLIC_URL` controls the link opened in a browser and defaults to `http://localhost:8100`; `GLANCE_SUBFORGE_INTERNAL_URL` is the gateway's container-to-container base URL and defaults to `http://subforge:8100`. `GLANCE_LIVE_SUBFORGE_INTERVAL` sets the gateway polling interval (default 2 seconds; accepted range 0.5–60 seconds).

Subforge is part of the regular Compose stack and starts after its selected local image has been built. After changing the token, rebuild the gateway and selected Subforge image, then recreate the three services so they receive the same value. The default CPU image can be built from the repository root with:

```shell
docker build --file subforge/Dockerfile --tag homelab-subforge:0.4.4-cpu subforge
docker compose build glance-live
docker compose up -d --force-recreate glance-live glance subforge
```

For CUDA, build the selected image and apply the same Compose override:

```shell
docker build --platform linux/amd64 --file subforge/Dockerfile.cuda --tag homelab-subforge:0.4.4-cuda subforge
docker compose -f docker-compose.yml -f compose/subforge.cuda.yml build glance-live
docker compose -f docker-compose.yml -f compose/subforge.cuda.yml up -d --force-recreate glance-live glance subforge
```

Run these commands from the repository root in PowerShell, Bash, or Zsh. Build
before applying the update and wait for active conversion jobs to finish;
Subforge shutdown cancels in-memory jobs. Recreating the service preserves the
named `subtitle_converter_data` volume. Do not use `down -v`.

Browser and gateway fixture checks, from the repository root:

```shell
node --test glance/test_subforge_live.cjs
cargo test --locked --manifest-path glance/live/Cargo.toml
```

After changing only either URL, recreate Glance. A `404` status means the widget endpoint is disabled; `401` means the tokens do not match. A connection error usually means Subforge is stopped or `GLANCE_SUBFORGE_INTERNAL_URL` cannot reach it on the Compose network.

## What it converts

The watcher scans movie and TV folders recursively for supported video files (`.mkv`, `.mks`, `.mp4`, `.m4v`, `.webm`, `.mov`, `.avi`, `.ts`, `.m2ts`) and sidecars (`.ass`, `.ssa`, `.vtt`, `.ttml`, `.smi`, `.sami`, `.sbv`, `.sup`, `.sub`, `.idx`). It exports matching subtitle tracks and converts sidecars to SRT beside the source when their wanted language and variant lack valid SRT coverage. Automatic selection chooses one source per missing variant and prefers text over OCR. Existing SRT files are not inputs, and generated files never replace existing files. For example, an embedded English track may produce `Example.en.srt`; explicitly selected duplicate or unknown tracks can receive track-number suffixes, and forced or hearing-impaired flags are retained in the name when available. Empty, correctly timed subtitle cues are removed from the converter's staged output before validation. A malformed SRT still fails validation and is not published. If a source later changes, an existing SRT is still left as-is; the converter does not refresh or overwrite it automatically.

Common text tracks keep their subtitle language and are converted with FFmpeg. TTML and SBV sidecars use the Subtitle Edit fallback converter; this path runs with OCR disabled too, because it converts text formats. Image-based tracks use the native Rust `ocr-rs`/MNN helper only when `CONVERTER_ENABLE_OCR=true`; OCR is disabled by default and **Settings → Preferences** shows whether the running service has OCR permission. The OCR language is inferred from embedded track metadata, a VobSub IDX language section, or a language suffix in a sidecar filename. `CONVERTER_OCR_LANGUAGE` selects the fallback model family for unknown image-subtitle language and defaults to `eng`. The 25 offline model files cover English, French/Latin, Arabic, and additional language families; there is no language detection from subtitle pixels. Unsupported language families fail with an explicit error. CPU OCR uses MNN CPU, while the CUDA edition can run the same model helper on an explicitly enabled GPU. It rejects a session that MNN silently creates on CPU when CUDA setup fails, though MNN may still run individual unsupported operators on CPU inside a CUDA session. OCR quality varies by font, resolution, and language; codecs and formats unsupported by the tools can still fail.

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
| `CONVERTER_OCR_LANGUAGE` | Offline OCR model family for unknown image-subtitle language | `eng` |
| `CONVERTER_BIND_ADDRESS` | Host interface for the dashboard publication | `127.0.0.1` |
| `CONVERTER_ALLOWED_HOSTS` | Comma-separated `Host` header allowlist; preserve the defaults and add your browser hostname if needed | `localhost,127.0.0.1,::1,subforge` |
| `CONVERTER_AUTH_ENABLED` | Enable HTTP Basic Authentication for dashboard routes; when false, the dashboard has no login | `true` |
| `CONVERTER_UI_PASSWORD` | Dashboard password (12–256 printable characters); blank generates and saves one in `/data/ui-password` | Blank |
| `CONVERTER_WEBHOOK_TOKEN` | Shared Sonarr/Radarr webhook token; blank disables the hook endpoints | Blank |
| `BAZARR_INTERNAL_URL` | Bazarr URL reachable from the converter on `homelab` | `http://bazarr:6767` |
| `BAZARR_API_KEY` | Bazarr API key; keep it private in `.env` | Blank |

The four `CONVERTER_*_DIR` paths are required Compose inputs. Authentication is enabled unless `CONVERTER_AUTH_ENABLED=false`; only when enabled does `CONVERTER_UI_PASSWORD` get read and validated or, when blank, generated and persisted. `CONVERTER_ALLOWED_HOSTS` may stay at its local/service-name default. For the CPU edition, `CONVERTER_ENABLE_OCR` is optional and defaults to false. The CUDA Compose override enables both OCR and GPU permission and supplies the NVIDIA visibility/capability settings; do not set worker-count environment variables, because worker allocation is saved from **Overview** in `/data/workers.conf`.

For example, to use `/srv/media/movies` as the first movie library on Linux, set `CONVERTER_MOVIES_DIR=/srv/media/movies` in `.env`; the container will still see it at `/movies`. On Windows, use a drive path available to Docker Desktop, such as the example `D:/film`. The mounted host folders must be writable by container UID/GID 1000 so the converter can create SRT sidecars. Keep them aligned with Bazarr and media-manager access. In Subforge, **Settings → Media folders** lets you select which mounted folders or subfolders to scan; add, browse, scan, or remove roots there. This selection is saved immediately and applies to future scans. Folder browsing is restricted to the configured mounts. Changing a `CONVERTER_*_DIR` changes the host folder behind its corresponding mount; changing container-side destinations instead requires updating the Compose mount and matching `subforge --root` arguments. The `subtitle_converter_data` named volume mounted at `/data` separately persists the generated dashboard password, settings, selected scan roots, job history, and scan state across rebuilds. `.env.example` keeps API-key entries blank; store real service keys and any chosen dashboard or webhook passwords only in the ignored `.env`.

## Status and resource readings

**Settings → System** has separate About, Live resources, and Execution limits panels. While **Overview** or **Settings → System** is visible in a visible browser tab, resource and worker readings refresh every three seconds; the displayed uptime advances locally once per second on System. These timers stop when neither view is visible or the tab is hidden. Dashboard status polling remains independent: it polls every three seconds while work is active, every 15 seconds while idle, and every 60 seconds in a hidden tab. Dashboard read-only GET requests time out after 15 seconds so a stuck request cannot prevent a later poll; successful samples restore the live state. Resource and worker indicators are marked stale after a failed or overdue sample and remain stale until a later read succeeds. The top bar's **Updated** time includes seconds. `/api/system` uses the dashboard's existing authentication behavior. Execution limits reflect effective worker admission, while **Overview** shows requested targets, ceilings, measured CPU/RAM headroom, active files, and connected worker nodes.

The readings are observations, not scheduling controls. On supported Linux Docker deployments, CPU and memory usage come from the container cgroup and include conversion child processes; Rust process RSS and thread count cover only the Subforge server process. CPU usage is shown as a percentage of the effective CPU allowance, where 100% means the full allowance (the panel also shows CPU equivalents used). The allowance reflects the visible cgroup quota and process CPU affinity. It may not include ancestor limits outside the container's visible cgroup namespace. The first CPU sample, a counter reset, or an unsupported source can leave a reading unavailable; the UI reports that state instead of showing a fabricated zero. Native Windows kernel resource readings are unavailable. Linux process RSS and thread readings may still be available when cgroup metrics are unavailable, such as a shared-host Linux setup whose cgroup namespace does not expose the process container root.

Container RAM is current cgroup usage against its reported memory ceiling, while process RAM is the Rust server's resident set only. The metadata cache shows its entry count and accounted payload bytes against the 5,000-entry and 32 MiB limits. Those byte counts are approximate serialized-payload bookkeeping, not actual heap usage or an additional memory reservation. The separate Library inventory remains limited to 2,000 inspected files; this inventory cap is unrelated to the metadata cache entry limit.

### Worker allocation

The Overview worker allocation panel reads and updates `/data/workers.conf`. The application generates this private JSON configuration file atomically when it is missing. Requested worker targets and the preserved explicit safety ceilings live in this file, not environment variables. An existing `workers.json` allocation is migrated when present. New configurations default to one CPU worker, one CPU OCR slot, no GPU workers, and a four-field worker pool that is not paused; active safety ceilings default to 32 CPU workers, 16 OCR workers, and 4 GPU workers. Older configured ceilings are retained when files are upgraded. The free-GPU-memory threshold defaults to 2048 MiB.

The UI saves requested targets immediately; they apply to newly admitted files while active files finish. `GET` and `PUT /api/workers` expose the authenticated allocation and live admission status. Requests are strict, but target values can be as high as 1,000,000. A target is a retained concurrency request, not a thread count or an instruction to spawn that many processes. Actual active work remains bounded by the saved `max_*_workers` ceilings, hard limits (32 CPU, 16 CPU OCR, 4 GPU), memory headroom, OCR permission, and GPU availability. CPU quota can oversubscribe requested concurrency and affects throughput; it does not itself change the saved target. OCR slots are a subset of CPU slots. Each external tool uses one CPU thread. Up to 32 file-scoped job coordinators can run concurrently; they all draw from the same live CPU/GPU worker admission pool, so raising the saved worker target lets queued files in any active job use newly available slots. Lowering a target stops new admissions above that target while already active files finish safely. Library-wide discovery, full inventory/inspection, and maintenance jobs remain exclusive. Files with equal or dot-suffixed stems in the same folder are serialized to protect shared output names.

Admission estimates reserve 128 MiB for the server, 256 MiB per CPU file worker, and 1 GiB per CPU OCR or GPU worker, then also checks current cgroup memory headroom when available. These estimates guide concurrency; they are not guaranteed peaks or reserved memory. Under memory pressure, new work waits and the UI shows the reason. **Pause local workers** stops admitting new files and lets active workers finish; queued work waits until resumed. This setting is separate from **Settings → Tasks → Pause schedules**. OCR workers are admitted only when image OCR is enabled. `CONVERTER_CPU_LIMIT` controls Docker's CPU ceiling independently. Changing it requires recreating the service; `docker compose restart` retains the existing container configuration. Use this generic command from the repository root on PowerShell, Bash, or Zsh:

```shell
docker compose up -d --force-recreate --no-deps subforge
```

This recreates only `subforge`, without rebuilding its image or starting dependencies.

### Optional remote worker nodes

The dashboard shows local and connected worker capacity under **Overview → Worker nodes**. Remote workers are opt-in. To enable registration, set the same private `CONVERTER_WORKER_TOKEN` on the coordinator and each worker; `.env.example` leaves it blank. Use a random printable token of 32–256 characters, keep it separate from the dashboard password, and recreate the coordinator after changing its Compose environment. The coordinator exposes only the bounded worker registration, heartbeat, lease, and completion API routes when this token is configured; their bearer-token authentication remains independent of the dashboard login setting.

Each worker must see the same media files and same-folder staging area as the coordinator. Configure an explicit JSON mapping for every shared root: `server` is the coordinator's mounted root path and `local` is that root as seen by the worker. The paths may differ between machines, but they must resolve to the same files. The worker validates mapped paths and stages outputs beside their sources; the coordinator validates the lease and source fingerprint before publishing completed SRTs. **Only use trusted worker hosts:** the shared media filesystem is writable by workers, so a worker process or host with filesystem access can modify mapped media and outputs directly. Protocol staging and coordinator publication checks do not isolate or protect the filesystem from a malicious or compromised worker. Use a trusted network or TLS-enabled reverse proxy when traffic crosses a network you do not trust. The coordinator URL must be an HTTP(S) origin with no path, query, fragment, or embedded credentials.

Set `CONVERTER_WORKER_TOKEN` directly through a local secret manager or protected process environment before launching the worker. Do not put the token value in a command, shell history, Compose file, or shared documentation. Example for a Bash or Zsh worker host, with the token already injected into its environment:

```shell
export CONVERTER_COORDINATOR_URL="https://subforge.example.invalid"
export CONVERTER_WORKER_MAPPINGS='[{"server":"/movies","local":"/srv/media/movies"},{"server":"/tv","local":"/srv/media/tv"}]'
export CONVERTER_WORKER_NAME="media-node"
subforge-worker
```

PowerShell equivalent, with `CONVERTER_WORKER_TOKEN` already injected into the process environment:

```powershell
$env:CONVERTER_COORDINATOR_URL = 'https://subforge.example.invalid'
$env:CONVERTER_WORKER_MAPPINGS = '[{"server":"/movies","local":"D:/Media/Movies"},{"server":"/tv","local":"D:/Media/TV"}]'
$env:CONVERTER_WORKER_NAME = 'media-node'
subforge-worker
```

Run the `subforge-worker` binary from the locally built CPU or CUDA image, or from a matching native build. Optional `CONVERTER_WORKER_CPU_SLOTS` (1–32), `CONVERTER_WORKER_OCR_SLOTS` (0–16 and no greater than CPU slots), and `CONVERTER_WORKER_GPU_SLOTS` (0–4) set its local targets. OCR and GPU slots also require the corresponding local runtime permission and resources. `CONVERTER_WORKER_DATA` selects its private state directory and defaults to `/data/worker`. Remote assignments use bounded leases and local memory admission; an unavailable or expired lease is retried locally. The coordinator accepts at most 64 registered nodes and 64 active leases. Workers stage outputs in private directories on the shared media filesystem; the coordinator validates results and publishes them only while the lease and source fingerprint remain valid. This mode requires explicitly mapped shared media paths that refer to the same files. Trust the network or use TLS across untrusted networks. Adding nodes does not bypass coordinator or worker resource limits.

### Optional NVIDIA GPU OCR

The default Compose service and CPU image expose no GPU devices. The optional edition is a separate `linux/amd64` image built with `Dockerfile.cuda` and the CUDA feature for native `ocr-rs`/MNN. CPU and CUDA test/runtime images contain no Python or PaddlePaddle stack. Both helpers and their test suites are Rust; native MNN is built from its pinned 3.4.1 source commit, and 25 model files come from an immutable source revision and are checked against `native-ocr/models.sha256`. A normal build needs network access for the MNN source and models. For an optional cache, place the pinned MNN archive `mnn-6b1db4c2eacf8193e6b7a1841acb6e31a1938e82.tar.gz` at the root of `output/ocr-cache` and any cached model files under `output/ocr-cache/models/`, using the exact filenames in `native-ocr/models.sha256`. Pass `--build-context native-source-cache=output/ocr-cache` to the Docker build. Every cached model is still checked against the manifest; a missing cache file is downloaded from the same immutable source URL and checked too. The CUDA build also fetches a pinned CUTLASS archive. Optionally cache `cutlass-319a389f42b776fae5701afcb943fc03be5b5c25.tar.gz` under `output/ocr-cutlass-cache` and pass `--build-context cuda-source-cache=output/ocr-cutlass-cache` to the CUDA build. Its SHA-256 is checked whether cached or downloaded, and CMake is directed to the verified source tree to avoid cloning its history. Build the CPU edition first because the CUDA image uses the matching `0.4.4-cpu` runtime base.

```shell
docker build --platform linux/amd64 --file subforge/Dockerfile.cuda --tag homelab-subforge:0.4.4-cuda subforge
```

The optional source-cache context can avoid downloading the pinned MNN archive and any supplied model files. Cached and downloaded MNN archives and model files are hash-verified before compilation or packaging. The separate CUDA source-cache context holds the optional pinned CUTLASS archive; its SHA-256 is `1d1eac5bcbcced5c1a7f3de433143c0ab269f07551865ba9aa0fcb8618e988c4`.

The native helper verifies its packaged offline models and returns OCR results through the compatibility command/JSON interface expected by Subtitle Edit; the `paddleocr` executable name in the image points to the same Rust binary and does not include PaddlePaddle. Arabic OCR output is converted from MNN visual order to logical order while preserving left-to-right runs and combining marks. GPU health checks isolate the backend child process's stdout so diagnostic output cannot corrupt its JSON response. A GPU OCR session is admitted only after CUDA backend and available-memory checks pass, so CPU-only sessions are not labelled as GPU work. MNN may still execute individual unsupported operators on CPU within an admitted GPU session. These implementation details make no OCR speedup claim.

This configuration is provided for compatible NVIDIA-enabled Docker hosts on Linux/amd64. Windows requires Docker Desktop's WSL 2 backend with NVIDIA GPU support enabled; macOS GPUs do not provide CUDA. These are platform requirements from the image and runtime configuration; this repository's configuration alone does not establish that a particular host, driver, or Docker setup works. Check [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html) and [Docker Desktop GPU support](https://docs.docker.com/desktop/features/gpu/) for host prerequisites.

The CPU and CUDA images are built separately; Compose never builds or pulls these local tags. Build CPU first because the CUDA Dockerfile reuses its runtime base, then build CUDA:

```shell
docker build --target final --tag homelab-subforge:0.4.4-cpu subforge
docker build --platform linux/amd64 --file subforge/Dockerfile.cuda --tag homelab-subforge:0.4.4-cuda subforge
```

The same commands work in Windows PowerShell:

```powershell
docker build --target final --tag homelab-subforge:0.4.4-cpu subforge
docker build --platform linux/amd64 --file subforge/Dockerfile.cuda --tag homelab-subforge:0.4.4-cuda subforge
```

Create the ignored root `.env` from `.env.example`, set the four host media directories, and start the CUDA edition with its override:

```shell
docker compose -f docker-compose.yml -f compose/subforge.cuda.yml up -d --no-deps subforge
```

The override grants only NVIDIA device `0`, with the `compute` and `utility` driver capabilities, and sets `CONVERTER_ENABLE_OCR=true`, `CONVERTER_ENABLE_GPU=true`, `NVIDIA_VISIBLE_DEVICES=0`, and `NVIDIA_DRIVER_CAPABILITIES=compute,utility`. It does not grant every GPU or the Docker socket. For direct `docker run`, set those four environment values and separately grant device access (for example `--gpus device=0`); environment variables alone do not expose a GPU. The CUDA-edition dashboard, API, sidebar, and header identify the compiled edition, while the Overview worker panel reports whether the runtime and device are eligible. The saved GPU worker allocation remains zero until you set **GPU OCR slots** to one. The default free-memory threshold in workers.conf is 2048 MiB. With the GPU worker allocation left at zero, Subforge starts no GPU OCR work and leaves device memory available to other GPU applications such as LM Studio. The threshold is checked before work, but Docker does not reserve or cap GPU VRAM. Keep the GPU worker count at one when sharing the device. The override configures a `1536m` container RAM ceiling by default; this is system memory, separate from device VRAM. See [Development checks](#development-checks) for the native smoke-test procedure.

To return to the CPU image, recreate the service from the ordinary Compose entrypoint without the CUDA override. On startup, Subforge clamps the saved worker allocation to the compiled edition's permissions and deployment limits, then persists the adjusted values, so GPU slots return to zero:

```shell
docker compose up -d --force-recreate --no-deps subforge
```

## Media server connections

The Settings page stores Sonarr, Radarr, and Bazarr URLs, API keys, and path mappings in a private file under `/data`. The browser receives only whether each API key is set; it never receives the saved key. Environment variables prefill the Arr and Bazarr connection details; Arr values can also be adjusted in Settings. Bazarr uses `BAZARR_INTERNAL_URL` (default `http://bazarr:6767`) and `BAZARR_API_KEY`; leave the API key blank in `.env.example` and keep the real value private in the ignored root `.env`. Bazarr is opt-in in Settings. Use **Test connection** after configuring each server. API failures are summarized using safe status/category messages; for example, HTTP 401/403 means the service rejected the configured credentials or access. After conversion, enabled Arr connections receive scan requests for matching media items. Path mappings translate paths in Arr webhook payloads into paths mounted in this container:

| Arr reports | Converter sees | Notes |
|---|---|---|
| Radarr `/media` | `/movies` | Add a Radarr path mapping in the dashboard. |
| Radarr `/media-C` | `/movies-C` | Add this mapping when using the second movie mount. |
| Sonarr `/tv` | `/tv` | Matches the first TV mount in Compose. |
| Sonarr `/tv-C` | `/tv-C` | Matches the second TV mount in Compose. |

If your Arr path differs, add a mapping under that server's Settings tab. Sonarr and Radarr webhooks require a shared token. Set `CONVERTER_WEBHOOK_TOKEN` in the ignored `.env`, then in each app's **Settings → Connect**, add a Webhook targeting `http://subforge:8100/hooks/sonarr` or `http://subforge:8100/hooks/radarr`. See the [Sonarr](https://wiki.servarr.com/sonarr/settings#connect) or [Radarr](https://wiki.servarr.com/radarr/settings#connect) connection guide. In the connection's advanced headers, add `X-Webhook-Token` with the same token. Enable file import/download, upgrade, and rename events, then use the app's **Test** action. The webhook accepts the Test event without queueing a job; supported media events queue an extraction. If a newly imported file is younger than `CONVERTER_MIN_AGE`, that attempt remains pending for the next background scan.

For Bazarr, configure and enable the integration in **Settings → Connections → Bazarr**. Bazarr language profiles are authoritative for wanted language, forced, and HI/SDH variants. Sync reads the profile list, paged movie and series records, and episodes in bounded series-ID batches. Subforge counts only non-empty, structurally valid local SRTs matching the required language and flags; the `missing_subtitles` indicator alone is insufficient. Supported tagged embedded tracks and sidecars can fill missing variants. Automatic work skips untagged tracks; inspect and select one manually when its language is unknown. To intentionally create another output for a variant already covered, select the track and enable **Allow another selected track for a language that already has SRT**. Old `.trackN.und.srt` sidecars are not coverage and are not removed automatically.

The Bazarr inventory is bounded to 20,000 media items and uses the configured API base with `/api/system/languages/profiles`, paged `/api/movies` and `/api/series`, and `/api/episodes` for bounded series-ID batches. Incomplete pages, failed requests, unsupported profile data, and exceeded limits fail closed: the previous snapshot is marked stale or unavailable and automatic conversion remains blocked until a successful sync. A successful fresh inventory with unconfirmed indexing is usable for conversion and displays a warning; stale/unavailable inventory and definite `reindex_failed` status remain blocked. Published SRT paths are validated and durably aggregated in `/data/publication-queue.json`; failed and cancelled jobs keep any partial outputs in this queue. Bazarr `scan-disk` notifications wait until the submitted job queue and all active jobs drain, then paths are deduplicated and sent in a scoped batch. Tracking is bounded to 20,000 paths, 8 MiB of path data, and 132 job receipts. Subforge records that a scan was attempted before sending it, so restart after an uncertain attempt does not blindly replay the PATCH. The scan request allows up to 120 seconds (within a 300-second batch); inventory reads allow 90 seconds, while lightweight requests retain their 8-second timeout. Raw socket timeout errors are normalized as timeouts. If a scan times out, the transport is lost after sending the PATCH, or Bazarr returns an unusable success response, the job remains converted with indexing pending until a fresh authenticated inventory read confirms all tracked published paths; Subforge does not blindly resend an uncertain PATCH. Explicit HTTP errors and connection-refused responses remain definite failures. **Check Bazarr indexing** is read-only and does not repeat the scan; pending verification persists across restart and clears only when a later sync sees every tracked output. If tracking limits prevent full confirmation, the history remains verification-limited. A whole-series scan may outlast the connection test, so do not re-extract existing SRTs to address indexing delay. When Bazarr is disabled, Subforge uses local filesystem mode. Bazarr may still apply its own configured search/download rules to actions it initiates independently.

Path mappings are required whenever Bazarr reports media paths differently from the converter's mounted paths. For example, if Bazarr sees `/media/Movies` while Subforge sees `/movies`, add the Bazarr mapping `/media/Movies` → `/movies`; add a separate mapping for TV if needed. Confirm the mapped roots refer to the same host files and that both containers can access them. Subforge does not assume a mapping based on folder names, and automatic scoped scans report no match if the paths cannot be reconciled. On Windows, map the server-reported path to the corresponding container path (such as `/movies`), rather than entering a host drive path as the local container path. On macOS/Linux, use the same container-path rule.

Jellyfin uses the existing `JELLYFIN_INTERNAL_URL` and `JELLYFIN_API_KEY` environment variables; the dashboard displays the connection as environment-managed and does not save it in its settings file. Requests use Jellyfin's current MediaBrowser authorization header with the API key, supporting recent Jellyfin servers. Enable the integration, test the connection, load libraries, select movie and series libraries, and enable the matching refresh toggles. New SRT files then trigger refreshes only within those selected library locations. **Refresh timing** chooses an immediate local metadata refresh or a filesystem update notification. If immediate lookup cannot identify the media item, the service sends a scoped filesystem notification instead. Use **Refresh selected libraries** to enumerate and refresh movie and episode items inside the selected libraries; the job supports cancellation and stops with a visible error at 20,000 items per library. These requests use Jellyfin's [item refresh API](https://github.com/jellyfin/jellyfin/blob/master/Jellyfin.Api/Controllers/ItemRefreshController.cs). HTTP 401/403 can indicate an unsupported authorization format, rejected/expired API key, or insufficient access. Verify the configured server URL and API key and review the safe connection diagnostic before changing credentials. The settings page cannot edit or store the Jellyfin key; after changing `JELLYFIN_API_KEY` in the ignored root `.env`, recreate the converter container so it receives the new value.

If Jellyfin uses different paths, add mappings on its Settings tab. A native Windows path such as `D:\Media\Movies` can map to the converter's `/movies` mount; `C:\Media\TV` can map to `/tv-C`. This also translates the newly created SRT paths back into the paths Jellyfin sees.

### Safe integration diagnostic

The Rust executable includes a read-only connection check for configured media integrations. Run it from the repository root with the service running:

```shell
docker compose exec subforge subforge integration-check bazarr
```

Replace `bazarr` with `sonarr`, `radarr`, or `jellyfin` to check another integration. The diagnostic uses the converter's configured credentials, prints only the provider, connection result, and—for Jellyfin—library/selection counts. It does not print server addresses, library names, API keys, or raw server responses. A nonzero exit status means the configured connection check failed; use the dashboard's safe diagnostic and service logs for next steps. This check verifies connectivity only and does not change server settings or queue scans.

## Memory use and OCR

Compose and `.env.example` give Subforge a `1536m` memory ceiling by default while keeping image OCR disabled. To enable image-track extraction, set `CONVERTER_ENABLE_OCR=true` in the private `.env` and recreate the service; the default ceiling already matches the OCR configuration. When OCR remains disabled, a text-focused deployment can set `CONVERTER_MEMORY_LIMIT=512m`, but TTML and SBV use Subtitle Edit even without image OCR, so validate the limit against your formats. **Settings → Preferences** reports whether image OCR is enabled; it does not change the environment-controlled permission. Every media-tool worker uses one native thread. File-level parallelism is bounded by requested worker targets, configured and hard ceilings, current memory admission, and resource availability as described in [Worker allocation](#worker-allocation). These estimates guide admission; they are not reserved host memory or guaranteed peak usage. Docker uses less than its ceiling when the workload needs less.

See the repository's [memory budget guide](../docs/memory-budget.md) for the other example workload ceilings, Nextcloud AIO child limits, and a dry-run-first, reversible helper. Measure actual peak use under your own workload and leave room for the host, Docker Desktop or the Docker daemon, and host applications such as LM Studio.

## Run the Rust application natively

For native development, install Rust and Cargo, FFmpeg/`ffprobe`, MKVToolNix/`mkvmerge`, and Subtitle Edit `seconv` v5.2.0. Image OCR additionally requires building the `native-ocr` helper with its pinned Rust dependencies and MNN build prerequisites, then installing the helper and checksum-verified models at the image paths. Docker images include these dependencies and models. See [FFmpeg downloads](https://ffmpeg.org/download.html), [MKVToolNix downloads](https://mkvtoolnix.download/downloads.html), and the [Subtitle Edit releases](https://github.com/SubtitleEdit/subtitleedit/releases).

PowerShell example with two Windows media roots and persistent application data:

```powershell
$subforgeData = Join-Path $env:LOCALAPPDATA 'Subforge'
New-Item -ItemType Directory -Force -Path $subforgeData | Out-Null
cargo run --manifest-path .\subforge\Cargo.toml -- --host 127.0.0.1 --data $subforgeData --root 'D:\Media\Movies' --root 'D:\Media\TV'
```

On macOS/Linux, use native paths and a user state directory:

```shell
subforge_data="${XDG_STATE_HOME:-$HOME/.local/state}/subforge"
mkdir -p "$subforge_data"
cargo run --manifest-path subforge/Cargo.toml -- --host 127.0.0.1 --data "$subforge_data" --root /srv/media/movies --root /srv/media/tv
```

The native server enables persisted scheduled tasks by default; pass `--no-auto-scan` to disable automatic scheduling. Set environment variables such as `CONVERTER_ENABLE_OCR`, `CONVERTER_OCR_LANGUAGE`, and the media-server integration URLs/keys in the shell environment before running. Keep API keys private.

## Development checks

Run the Rust test suite from the repository root:

```shell
cargo test --locked --manifest-path subforge/Cargo.toml
```

Build the production binary without updating locked dependencies:

```shell
cargo build --locked --release --manifest-path subforge/Cargo.toml
```

The locked Rust dependencies use rustls 0.23.46, which fixes RustSec advisories RUSTSEC-2026-0346, -0347, -0348, and -0349 ([upstream release](https://github.com/rustls/rustls/releases/tag/v%2F0.23.46), [RustSec advisory database](https://rustsec.org/)). Existing installations need a rebuilt image to receive the dependency fix. The rustls change is a dependency security fix; this release also updates OCR and job scheduling as described above. Current checks pass for both strict locked RustSec audits (130 application and 157 native-OCR lockfile packages) and workflow linting. All 11 native helper tests pass with strict Clippy, including Arabic visual-to-logical ordering; direct CPU recognition has produced the expected exact English (`HELLO WORLD`), French (`L'ÉTÉ À PARIS`), and Arabic (`مرحبا بالعالم`) fixture text. The full CPU SUP-to-SRT extraction pipeline now passes for English, French, and Arabic, and confirms existing SRT files are preserved. The Rust HTTP text-conversion and 512 MiB resource smokes pass; OCR also passes within a 1.5 GiB memory limit. Direct CUDA recognition in the deployed image has produced the exact English, French, and Arabic fixture text. The full CUDA SUP-to-SRT smoke passes in an isolated test container for 200 English cues plus French and Arabic SUP fixtures, observing the CUDA backend and GPU worker, expected text, source preservation, and a cached repeat. Runtime performance benchmarking remains pending.

The CPU and CUDA Dockerfiles build the Rust app, OCR helper, and test-only `subforge-smoke` CLI. The `test` target is a disposable fixture image. The CPU `extraction` procedure generates SUP image-subtitle fixtures with Subtitle Edit for English, French, and Arabic, runs them through the service, checks expected script/text in the resulting SRTs, and confirms an existing SRT and the SUP inputs remain unchanged; this full CPU SUP-to-SRT pipeline has passed. `extraction --text-only` checks supported text conversions and preservation without image OCR. Build the test image and run these checks without network access:

```shell
docker build --target test --tag homelab-subforge:0.4.4-cpu-test subforge
docker run --rm --network none --entrypoint /usr/local/bin/subforge-smoke homelab-subforge:0.4.4-cpu-test extraction
docker run --rm --network none --entrypoint /usr/local/bin/subforge-smoke homelab-subforge:0.4.4-cpu-test extraction --text-only
```

For the full CUDA SUP-to-SRT smoke on a compatible Linux/amd64 host, build the CUDA test target and expose only NVIDIA device 0. Direct native CUDA recognition in the deployed image matched the exact English (`HELLO WORLD`), French (`L'ÉTÉ À PARIS`), and Arabic (`مرحبا بالعالم`) fixture text. The full GPU smoke has also passed in an isolated test container: 200 English cues plus French and Arabic SUP fixtures were recognized with the CUDA backend and GPU worker observed, sources preserved, and a repeated extraction served from the OCR cache. Runtime performance benchmarking remains pending. To repeat the smoke:

```shell
docker build --platform linux/amd64 --file subforge/Dockerfile.cuda --target test --tag homelab-subforge:0.4.4-cuda-test subforge
docker run --rm --gpus device=0 --network none --entrypoint /usr/local/bin/subforge-smoke homelab-subforge:0.4.4-cuda-test gpu --cues 200
```

The same single-line `docker build` and `docker run` commands work in Windows PowerShell with Docker Desktop WSL 2 GPU support enabled. CPU and CUDA test targets contain no Python runtime. GPU behavior depends on the host and driver; the fixture smoke establishes this pipeline on the tested host, while performance on a particular workload remains unmeasured.
