# Subtitle Studio

Subtitle Studio is a local Rust web app for creating timed `.srt` subtitles from MKV, MP4, M4V, and WebM media. The Rust server handles the browser UI, bounded job queue, transcript cache, translation, and review. It invokes FFmpeg and the native `whisper-cli` executable as child processes. n8n is independent.

The application and command-line utilities require no Python interpreter. FFmpeg and whisper.cpp remain native C/C++ dependencies; the browser uses bundled HTML, CSS and JavaScript.

### Upgrade an existing Compose installation

The Compose service and container were formerly named `subtitle-worker`; the new name is `subtitle-studio`. Docker does not rename an existing container's Compose labels. Build the selected new image and back up its data. Inspect the exact old container and confirm its Compose project, service label, and mounted volume names before stopping it. Do not remove a similar-looking container unless inspection confirms it belongs to this installation. Use the same Compose project name shown by inspection in every migration Compose command; `homelab` is the standard example. If the existing installation uses a custom project name, substitute that exact name below so it continues to resolve the same `subtitle_data` and `subtitle_models` volume keys:

```shell
docker inspect subtitle-worker --format '{{.Name}} {{index .Config.Labels "com.docker.compose.project"}} {{index .Config.Labels "com.docker.compose.service"}} {{range .Mounts}}{{.Destination}} {{.Type}} {{.Name}}; {{end}}'
docker volume ls
docker stop subtitle-worker
docker rm subtitle-worker
docker compose --project-name homelab up -d --no-deps --build subtitle-studio
```

For CUDA, keep the same inspected project name and `-f compose/subtitle-studio.cuda.yml` on the build, download, and start commands. For example, for a standard `homelab` project the migrated service start is:

```shell
docker compose --project-name homelab -f docker-compose.yml -f compose/subtitle-studio.cuda.yml up -d --no-deps subtitle-studio
```

After removal, verify the inspected volumes still exist. Do not run `down -v` or `--remove-orphans` for this rename. Removing the former container leaves its volumes in place. Existing `SUBTITLE_*` settings remain supported. For container-to-container access, update an explicit `GLANCE_SUBTITLE_STUDIO_INTERNAL_URL` from the old service host to `subtitle-studio`. If `SUBTITLE_ALLOWED_HOSTS` explicitly contains the old service name, replace it only if clients actually send `subtitle-studio` as the HTTP Host value; the app does not widen an explicit allowlist automatically.

## Resource defaults

The default Compose service is CPU-only: `WHISPER_MODEL=small-q5_1`, `WHISPER_DEVICE=cpu`, two Whisper threads, one pipeline worker, and 300-second WAV chunks. The worker releases the Whisper model between chunks before translation or review. Compose sets a 1 GiB memory ceiling, 2 CPU quota, and 64 process ceiling. These are configured limits, not measured use or a guarantee that every workload fits. Longer media, translation models loaded by LM Studio, FFmpeg, and filesystem cache use additional host resources outside this worker's CPU/GPU settings.

Whisper downloads are disabled by default (`WHISPER_LOCAL_FILES_ONLY=true`). Download the selected GGML model explicitly:

```shell
docker compose run --rm subtitle-studio download-model small-q5_1
```

For the full `large-v3` model on an NVIDIA GPU, use the CUDA Compose override on every build, download, start, and later Compose command. The default image is CPU-only and does not request a GPU. The CUDA override uses a separate local image tag, `homelab-subtitle-studio:1.0.0-cuda`. Add these values to the ignored root `.env` (replace the allowed host list with the browser-facing names you use):

```dotenv
WHISPER_MODEL=large-v3
WHISPER_DEVICE=cuda
WHISPER_LOCAL_FILES_ONLY=true
SUBTITLE_CUDA_ARCHITECTURES=86
SUBTITLE_MODEL_DOWNLOAD_TIMEOUT=3600
SUBTITLE_STUDIO_MEMORY_LIMIT=6g
SUBTITLE_STUDIO_CPUS=2
SUBTITLE_CPU_THREADS=2
SUBTITLE_AUDIO_CHUNK_SECONDS=300
```

`large-v3` is the full precision GGML model; the [upstream model repository](https://huggingface.co/ggerganov/whisper.cpp/tree/main) lists its file at about 3.1 GB. Download it once into the persistent model volume, then start the service:

```shell
docker compose -f docker-compose.yml -f compose/subtitle-studio.cuda.yml build subtitle-studio
docker compose -f docker-compose.yml -f compose/subtitle-studio.cuda.yml run --rm subtitle-studio download-model large-v3
docker compose -f docker-compose.yml -f compose/subtitle-studio.cuda.yml up -d subtitle-studio
```

`SUBTITLE_CUDA_ARCHITECTURES=86` builds for an RTX 3060's CUDA compute capability; omit it on another GPU to use the override's generic architecture list, or set the value supported by your target GPU. Rebuild the CUDA image after changing this value. `SUBTITLE_MODEL_DOWNLOAD_TIMEOUT` applies to an explicit `download-model` command only; its default is 3600 seconds. Web jobs that need a missing model keep their shorter timeout and can be cancelled. Model downloads are streamed to a temporary file and installed atomically. Keep `WHISPER_LOCAL_FILES_ONLY=true` so a web job never starts an implicit download. A full model can use substantially more RAM and VRAM than the small quantized CPU default. The suggested 6 GiB container ceiling is a starting cap; one short isolated `large-v3` validation recorded a 369.2 MiB kernel memory peak under that limit, but this does not establish a minimum for longer media or concurrent workloads. Docker RAM and GPU VRAM are separate resources. An RTX 3060 with 12 GiB of VRAM shares that memory with LM Studio and other GPU processes. Subtitle Studio unloads Whisper before requesting translation/review from LM Studio, but other applications may retain their allocations. The same short run observed 5102 MiB whole-device VRAM, including other applications. See the [full validation measurements](../docs/memory-budget.md#subtitle-studio-full-large-v3-cuda-validation); measure actual peak use with representative media before lowering the ceiling or increasing concurrent work.

When `WHISPER_DEVICE=cuda`, Subtitle Studio verifies that whisper.cpp selected and initialized CUDA for each audio chunk. If the CUDA backend cannot initialize or allocate its model state, that chunk fails with a clear CUDA error; the app will not silently run it on CPU. The error advises checking the CUDA image, GPU reservation and host driver. For a custom `WHISPER_CLI`, use a build matching the pinned whisper.cpp version and its initialization output format. Raw Whisper stderr is drained and discarded; it is not retained in logs or returned to the browser.

Model files live in the persistent `/models` volume. These GGML files use the whisper.cpp model format and cannot reuse the old Hugging Face Faster Whisper cache. Back up `/data`, subtitle outputs, and any needed transcript data before replacing images or volumes. Existing job history and outputs remain in their mounts; inference caches are regenerated as needed.

The image runs as UID/GID `1000:1000`. Keep `/data`, `/models` and the host output folder writable by that identity. The repeated “Could not create temporary file” startup log can occur when an existing `/data` volume is owned by root: the app needs to atomically replace its jobs file during startup. The screenshot alone cannot establish which mount has the wrong permissions. Check the startup log for the path-free storage diagnostic, then verify ownership and access for `/data` and `/media/output` before changing permissions.

For an older root-owned **named volume**, stop Subtitle Studio, make a backup, and inspect the stopped container's mounts. `docker inspect subtitle-studio --format '{{range .Mounts}}{{println .Destination .Type .Name .Source}}{{end}}'` shows each destination, mount type, and volume name or bind source. Only if `/data` or `/models` is confirmed to be the affected named volume, set the volume variable to that exact inspected name and repair that one mount. PowerShell example (repeat separately for `/models` only if needed):

```powershell
docker compose stop subtitle-studio
$studioImage = docker inspect subtitle-studio --format '{{.Config.Image}}'
$dataVolume = '<verified-volume-mounted-at-/data>'
$modelVolume = '<verified-volume-mounted-at-/models>'
docker run --rm --pull=never --network none --read-only --cap-drop ALL --cap-add CHOWN --security-opt no-new-privileges:true --user 0:0 --mount "type=volume,source=$dataVolume,target=/data" --entrypoint chown $studioImage -R -h -P -- 1000:1000 /data
docker run --rm --pull=never --network none --read-only --cap-drop ALL --cap-add CHOWN --security-opt no-new-privileges:true --user 0:0 --mount "type=volume,source=$modelVolume,target=/models" --entrypoint chown $studioImage -R -h -P -- 1000:1000 /models
```

Bash or zsh:

```bash
docker compose stop subtitle-studio
studio_image=$(docker inspect subtitle-studio --format '{{.Config.Image}}')
data_volume='<verified-volume-mounted-at-/data>'
model_volume='<verified-volume-mounted-at-/models>'
docker run --rm --pull=never --network none --read-only --cap-drop ALL --cap-add CHOWN --security-opt no-new-privileges:true --user 0:0 --mount "type=volume,source=$data_volume,target=/data" --entrypoint chown "$studio_image" -R -h -P -- 1000:1000 /data
docker run --rm --pull=never --network none --read-only --cap-drop ALL --cap-add CHOWN --security-opt no-new-privileges:true --user 0:0 --mount "type=volume,source=$model_volume,target=/models" --entrypoint chown "$studio_image" -R -h -P -- 1000:1000 /models
```

Replace each placeholder only with the exact volume name printed for that mount; leave out the command for any mount that does not need repair. These commands require a backup first and do not apply to a host bind mount; use the host filesystem's permissions for bind-mounted paths. The helper runs offline with a read-only root and only the `CHOWN` capability. If directory traversal fails because directories are mode `0700`, add `--cap-add DAC_READ_SEARCH` to that helper invocation only. Do not use `down -v` as part of this migration. Interrupted tasks are marked failed on restart; completed snapshots and matching saved transcript sources remain usable for downloads and later reviews. Each job has a snapshot under `/data/job_outputs/<job_id>/`; conventional exports under `/media/output` may be replaced by a later job for the same media/language.

## Run with Docker Compose

Copy `.env.example` to `.env`, then set `SUBTITLE_LIBRARY_DIR` and `SUBTITLE_MEDIA_DIR` to directories on the host. These variables are host-side bind mount paths; inside the container the source is `/library` (read-only) and the output is `/media/output`. The default host publication is `127.0.0.1:8099`.

```shell
docker compose up -d --build subtitle-studio
```

Open <http://localhost:8099>. Select a video, audio track, source language, model, and optional target languages. Translation and quality review use the configured LM Studio compatible API. Source-only transcription does not require an LLM endpoint. The app has no authentication; keep it on loopback or restrict access to trusted clients.

The server checks every HTTP `Host` header against `SUBTITLE_ALLOWED_HOSTS` to reduce DNS rebinding risk. Its default allowlist is `localhost,127.0.0.1,::1,subtitle-studio,subtitle-worker`; the former service alias remains accepted for compatibility. Access through another hostname or IP requires adding that exact name or address to the comma-separated setting in `.env`; a non-loopback bind by itself does not allow that Host. Only list names or addresses through which trusted clients should reach the app.

The default service requires no GPU. On a supported NVIDIA host, the optional CUDA build is provided by `compose/subtitle-studio.cuda.yml`. The override both builds the CUDA-capable image and requests one GPU; merely setting `WHISPER_DEVICE=cuda` on the default image is insufficient:

```shell
docker compose -f docker-compose.yml -f compose/subtitle-studio.cuda.yml build subtitle-studio
docker compose -f docker-compose.yml -f compose/subtitle-studio.cuda.yml up -d subtitle-studio
```

That image compiles whisper.cpp v1.8.3 at the pinned upstream commit. CUDA support requires NVIDIA drivers and the [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html). The Compose device request follows [Docker's GPU reservation configuration](https://docs.docker.com/compose/how-tos/gpu-support/). Docker Desktop GPU passthrough is supported through its Windows WSL 2 backend; Docker Desktop on macOS does not expose NVIDIA CUDA. Apple GPU acceleration is not configured by this optional CUDA image.

## Native run

Install Rust 1.90 or newer, FFmpeg (including `ffprobe`), and `whisper-cli`; make the native executables available on `PATH`. On Windows, avoid FFmpeg shim launchers for subprocess tools; use direct executable paths so the worker can stop child processes reliably. Build the Rust app from this directory:

```shell
cargo build --release --locked
```

Set `SUBTITLE_LIBRARY_ROOT`, `SUBTITLE_OUTPUT_ROOT`, `SUBTITLE_DATA_ROOT`, and `WHISPER_MODEL_ROOT` to writable or readable directories as appropriate. The app reads environment variables directly and does not load the repository `.env` automatically. Example PowerShell:

```powershell
$env:SUBTITLE_LIBRARY_ROOT = Join-Path $PWD 'library'
$env:SUBTITLE_OUTPUT_ROOT = Join-Path $PWD 'output'
$env:SUBTITLE_DATA_ROOT = Join-Path $PWD 'data'
$env:WHISPER_MODEL_ROOT = Join-Path $PWD 'models'
$env:WHISPER_MODEL = 'small-q5_1'
$env:WHISPER_LOCAL_FILES_ONLY = 'true'
New-Item -ItemType Directory -Force -Path $env:SUBTITLE_LIBRARY_ROOT, $env:SUBTITLE_OUTPUT_ROOT, $env:SUBTITLE_DATA_ROOT, $env:WHISPER_MODEL_ROOT | Out-Null
& .\target\release\subtitle-studio.exe download-model small-q5_1
& .\target\release\subtitle-studio.exe serve
```

For macOS/Linux Bash or zsh, from `subtitle-studio` after building:

```bash
export SUBTITLE_LIBRARY_ROOT="$HOME/subtitle-studio-example/library"
export SUBTITLE_OUTPUT_ROOT="$HOME/subtitle-studio-example/output"
export SUBTITLE_DATA_ROOT="$HOME/subtitle-studio-example/data"
export WHISPER_MODEL_ROOT="$HOME/subtitle-studio-example/models"
export WHISPER_MODEL=small-q5_1
export WHISPER_LOCAL_FILES_ONLY=true
mkdir -p "$SUBTITLE_LIBRARY_ROOT" "$SUBTITLE_OUTPUT_ROOT" "$SUBTITLE_DATA_ROOT" "$WHISPER_MODEL_ROOT"
./target/release/subtitle-studio download-model small-q5_1
./target/release/subtitle-studio serve
```

The service binds to `127.0.0.1:8099` by default. `SUBTITLE_BIND_ADDRESS` and `SUBTITLE_PORT` configure the listener. Native recipes require the compiled helper executables on `PATH`; live native Whisper inference has not been verified on every host platform.

## Configuration

Compose maps `SUBTITLE_LIBRARY_DIR` and `SUBTITLE_MEDIA_DIR` from host paths. Application variables and defaults:

| Variable | Default | Purpose |
| --- | --- | --- |
| `SUBTITLE_BIND_ADDRESS`, `SUBTITLE_PORT` | `127.0.0.1`, `8099` | Native listener; Compose binds the container listener internally and publishes the host port using `SUBTITLE_BIND_ADDRESS`. |
| `SUBTITLE_ALLOWED_HOSTS` | `localhost,127.0.0.1,::1,subtitle-studio` | Comma-separated HTTP Host header allowlist checked on every request. Add the browser-facing hostname or IP for intentional remote access. |
| `SUBTITLE_STUDIO_MEMORY_LIMIT` | `1g` | Compose RAM ceiling. If unset, Compose accepts the legacy `SUBTITLE_WORKER_MEMORY_LIMIT`; when both are set, the new name takes precedence. |
| `SUBTITLE_STUDIO_CPUS` | `2` | Compose CPU ceiling. If unset, Compose accepts the legacy `SUBTITLE_WORKER_CPUS`; when both are set, the new name takes precedence. |
| `SUBTITLE_LIBRARY_ROOT` | `./library` native; `/library` in Compose | Input library root. |
| `SUBTITLE_OUTPUT_ROOT` | `./output` native; `/media/output` in Compose | Subtitle output root. |
| `SUBTITLE_DATA_ROOT` | `./data` native; `/data` in Compose | Persistent jobs and transcript data. |
| `WHISPER_MODEL_ROOT` | `./models` native; `/models` in Compose | GGML model directory. |
| `WHISPER_MODEL` | `small-q5_1` | Default Whisper model. |
| `WHISPER_DEVICE` | `cpu` | `cpu` by default; `cuda` or `metal` requires a whisper-cli built with that backend. |
| `SUBTITLE_CUDA_ARCHITECTURES` | `75;80;86;89` in the CUDA override | Compose build argument, not a runtime setting. Set it to the target GPU's CUDA compute capability to avoid building unrelated architectures. |
| `WHISPER_CLI` | `whisper-cli` | Whisper.cpp CLI executable. |
| `WHISPER_LOCAL_FILES_ONLY` | `true` | Require an explicit model download before inference. |
| `SUBTITLE_MODEL_DOWNLOAD_TIMEOUT` | `3600` seconds | Timeout for explicit CLI `download-model`; bounded to 1–86400 seconds. Automatic job downloads use fixed 120-second body and 180-second total timeouts per upstream URL. |
| `SUBTITLE_CPU_THREADS` | `2` | Whisper CPU thread count. |
| `SUBTITLE_AUDIO_CHUNK_SECONDS` | `300` | Maximum extracted WAV chunk duration. |
| `SUBTITLE_PROCESS_TIMEOUT` | `3600` | FFmpeg/Whisper process timeout in seconds. |
| `SUBTITLE_LLM_TIMEOUT` | `600` | Translation/review request timeout in seconds. |
| `SUBTITLE_MAX_JOBS` | `500` | Retained job history ceiling. |
| `SUBTITLE_MAX_QUEUE` | `32` | Waiting job ceiling. |
| `SUBTITLE_MAX_CLIENTS` | `16` | Accepted HTTP client ceiling. |
| `SUBTITLE_MAX_STREAMS` | `8` | Active event stream ceiling; must be lower than `SUBTITLE_MAX_CLIENTS`. |
| `SUBTITLE_MAX_JSON_BYTES` | `16777216` | Maximum saved JSON/cache and upstream response size. Browser request bodies have a separate fixed 16 KiB ceiling. |
| `SUBTITLE_LLM_BASE_URL` | `http://127.0.0.1:1234/v1` native; host gateway in Compose | LM Studio compatible API base URL. |
| `SUBTITLE_LLM_MODEL`, `SUBTITLE_REVIEW_MODEL` | empty | Default translation/review model IDs; empty selects from available models. |

`WHISPER_MODEL_ROOT` uses GGML model names from the app's allowlist, such as `small-q5_1`. Model downloads access the configured upstream model source; inference can run offline after download when local-only mode remains enabled. LLM requests send transcript text to the configured endpoint. Review and translation use that endpoint and its model loading behavior; this Rust worker does not allocate LM Studio GPU memory.

Open **Resources** below the Whisper model selector and use **Refresh usage** to sample resource use without background polling. The same data is available at <http://localhost:8099/api/resources> and `/api/config`. On Linux, the endpoint reports server process RSS, container cgroup memory use/limit, its kernel-recorded memory peak and CPU use when available; unsupported measurements are `null`. The memory peak covers the current container lifetime, including native helpers and file cache; refreshing does not reset it. CPU percentage is the average between successive resource requests, with 100% representing one core; the first request has no interval yet. It also reports cumulative CPU seconds, configured Whisper threads, device, chunk duration, job history and queue size. In CUDA mode it queries `nvidia-smi` with a three-second timeout for whole-device utilization and VRAM, including other applications; unavailable GPU measurements are `null`. LM Studio manages its own model allocation. These are sampled observations and configured values, not a resource guarantee.

An existing ignored `.env` can preserve `WHISPER_MODEL=medium` even though the new example defaults to `small-q5_1`. After backing up `/data` and outputs, explicitly set `WHISPER_MODEL=small-q5_1` and `WHISPER_LOCAL_FILES_ONLY=true` in `.env` if you want the CPU-first defaults. Existing GGML files must be named `ggml-<model>.bin`; Faster Whisper Hugging Face caches cannot be reused. Explicit CLI downloads use `SUBTITLE_MODEL_DOWNLOAD_TIMEOUT` (3600 seconds by default), while automatic web-job downloads keep 120-second body and 180-second total timeouts per upstream URL. For large or slow downloads, use the explicit CLI command with an adequate timeout or obtain the official `.bin` from [the whisper.cpp model repository](https://huggingface.co/ggerganov/whisper.cpp/tree/main) and place it in `WHISPER_MODEL_ROOT` (or `/models` in the container) before inference.

### Whisper JSON failures

Transcription requests ordinary Whisper JSON with segment text and timestamps. Full token JSON can contain incomplete UTF-8 byte sequences inside individual tokens, causing `Invalid Whisper JSON output` even after successful inference. Builds with this fix omit the unused token metadata while preserving existing transcript cache identities. Rebuild the selected CPU or CUDA image and use **Retry** on the failed transcription step. Remaining parse failures report only the encoding/syntax category and line/column, without exposing transcript text or native paths.

## Development and validation

From this directory:

```shell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo clippy --all-targets --features fixture-probe --locked -- -D warnings
node --check web/app.js
```

The Tailwind stylesheet is checked in. After changing its source classes, regenerate it with `pnpm build:css` when Node and pnpm are installed. These checks do not measure runtime memory or establish transcription quality on every platform. Use `docker stats --no-stream subtitle-studio` and host GPU tools with representative media to measure the deployment you run.

The optional Docker `test` target includes the Rust `subtitle-studio-probe` executable. It expects the public whisper.cpp JFK speech fixture converted to `/library/sample.mkv` and an already downloaded `tiny-q5_1` model by default; pass `--model large-v3` to test the full GPU model. The CUDA test target also includes the unit-test executable for unprivileged Linux permission fixtures. Run it beside the server in an isolated container without network access; it checks actual transcription, SRT download and cached transcript reuse. It is excluded from the final production image. The recorded short CPU check and its limits are in [the memory budget guide](../docs/memory-budget.md).

For standalone SRT checks, run `subtitle-studio validate-srt FILE --source ORIGINAL.srt --video VIDEO.mkv` (the source/video options are optional). It returns a nonzero status for invalid structure or mismatched timing. Saved output selection also supports `validate-srt --job-id ID --target-language en`. Model inference quality and peak resource use require representative media. Cancellation stops native processing promptly; a translation/review request already sent to LM Studio can take until its response or `SUBTITLE_LLM_TIMEOUT` before cancellation completes.
