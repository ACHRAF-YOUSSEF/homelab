# Subtitle Studio

Subtitle Studio is a local Rust web app for creating timed `.srt` subtitles from MKV, MP4, M4V, and WebM media. The Rust server handles the browser UI, bounded job queue, transcript cache, translation, and review. It invokes FFmpeg and the native `whisper-cli` executable as child processes. n8n is independent.

The application and command-line utilities require no Python interpreter. FFmpeg and whisper.cpp remain native C/C++ dependencies; the browser uses bundled HTML, CSS and JavaScript.

## Resource defaults

The default Compose service is CPU-only: `WHISPER_MODEL=small-q5_1`, `WHISPER_DEVICE=cpu`, two Whisper threads, one pipeline worker, and 300-second WAV chunks. The worker releases the Whisper model between chunks before translation or review. Compose sets a 1 GiB memory ceiling, 2 CPU quota, and 64 process ceiling. These are configured limits, not measured use or a guarantee that every workload fits. Longer media, translation models loaded by LM Studio, FFmpeg, and filesystem cache use additional host resources outside this worker's CPU/GPU settings.

Whisper downloads are disabled by default (`WHISPER_LOCAL_FILES_ONLY=true`). Download the selected GGML model explicitly:

```shell
docker compose run --rm subtitle-worker download-model small-q5_1
```

Model files live in the persistent `/models` volume. These GGML files use the whisper.cpp model format and cannot reuse the old Hugging Face Faster Whisper cache. Back up `/data`, subtitle outputs, and any needed transcript data before replacing images or volumes. Existing job history and outputs remain in their mounts; inference caches are regenerated as needed.

The image runs as UID/GID `1000:1000`. Keep `/data`, `/models` and the host output folder writable by that identity. For older root-owned named volumes, stop the worker, back them up, inspect the stopped container's mounts and migrate ownership only on the verified data/model volumes before restarting. Do not remove volumes with `down -v` as part of this migration. Interrupted tasks are marked failed on restart; completed snapshots and matching saved transcript sources remain usable for downloads and later reviews. Each job has a snapshot under `/data/job_outputs/<job_id>/`; conventional exports under `/media/output` may be replaced by a later job for the same media/language.

## Run with Docker Compose

Copy `.env.example` to `.env`, then set `SUBTITLE_LIBRARY_DIR` and `SUBTITLE_MEDIA_DIR` to directories on the host. These variables are host-side bind mount paths; inside the container the source is `/library` (read-only) and the output is `/media/output`. The default host publication is `127.0.0.1:8099`.

```shell
docker compose up -d --build subtitle-worker
```

Open <http://localhost:8099>. Select a video, audio track, source language, model, and optional target languages. Translation and quality review use the configured LM Studio compatible API. Source-only transcription does not require an LLM endpoint. The app has no authentication; keep it on loopback or restrict access to trusted clients.

The server checks every HTTP `Host` header against `SUBTITLE_ALLOWED_HOSTS` to reduce DNS rebinding risk. Its default allowlist is `localhost,127.0.0.1,::1,subtitle-worker`. Access through another hostname or IP requires adding that exact name or address to the comma-separated setting in `.env`; a non-loopback bind by itself does not allow that Host. Only list names or addresses through which trusted clients should reach the app.

The default service requires no GPU. On a supported NVIDIA host, the optional CUDA build is provided by `compose/subtitle-worker.cuda.yml`:

```shell
docker compose -f docker-compose.yml -f compose/subtitle-worker.cuda.yml build subtitle-worker
docker compose -f docker-compose.yml -f compose/subtitle-worker.cuda.yml up -d subtitle-worker
```

That image compiles whisper.cpp v1.8.3 at the pinned upstream commit. CUDA support requires NVIDIA drivers and the NVIDIA Container Toolkit. Docker Desktop GPU passthrough is supported through its Windows WSL 2 backend; Docker Desktop on macOS does not expose NVIDIA CUDA. Apple GPU acceleration is not configured by this optional CUDA image.

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

For macOS/Linux Bash or zsh, from `subtitle-worker` after building:

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
| `SUBTITLE_ALLOWED_HOSTS` | `localhost,127.0.0.1,::1,subtitle-worker` | Comma-separated HTTP Host header allowlist checked on every request. Add the browser-facing hostname or IP for intentional remote access. |
| `SUBTITLE_LIBRARY_ROOT` | `./library` native; `/library` in Compose | Input library root. |
| `SUBTITLE_OUTPUT_ROOT` | `./output` native; `/media/output` in Compose | Subtitle output root. |
| `SUBTITLE_DATA_ROOT` | `./data` native; `/data` in Compose | Persistent jobs and transcript data. |
| `WHISPER_MODEL_ROOT` | `./models` native; `/models` in Compose | GGML model directory. |
| `WHISPER_MODEL` | `small-q5_1` | Default Whisper model. |
| `WHISPER_DEVICE` | `cpu` | `cpu` by default; `cuda` or `metal` requires a whisper-cli built with that backend. |
| `WHISPER_CLI` | `whisper-cli` | Whisper.cpp CLI executable. |
| `WHISPER_LOCAL_FILES_ONLY` | `true` | Require an explicit model download before inference. |
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

Open **Resources** below the Whisper model selector and use **Refresh usage** to sample resource use without background polling. The same data is available at <http://localhost:8099/api/resources> and `/api/config`. On Linux, the endpoint reports server process RSS, container cgroup memory use/limit and CPU use when available; unsupported measurements are `null`. CPU percentage is the average between successive resource requests, with 100% representing one core; the first request has no interval yet. It also reports cumulative CPU seconds, configured Whisper threads, device, chunk duration, job history and queue size. In CUDA mode it queries `nvidia-smi` with a three-second timeout for whole-device utilization and VRAM, including other applications; unavailable GPU measurements are `null`. LM Studio manages its own model allocation. These are sampled observations and configured values, not a resource guarantee.

An existing ignored `.env` can preserve `WHISPER_MODEL=medium` even though the new example defaults to `small-q5_1`. After backing up `/data` and outputs, explicitly set `WHISPER_MODEL=small-q5_1` and `WHISPER_LOCAL_FILES_ONLY=true` in `.env` if you want the CPU-first defaults. Existing GGML files must be named `ggml-<model>.bin`; Faster Whisper Hugging Face caches cannot be reused. A model download has a 120-second body receive timeout and a 180-second total timeout per upstream URL. For large or slow downloads, obtain the official `.bin` from [the whisper.cpp model repository](https://huggingface.co/ggerganov/whisper.cpp/tree/main) and place it in `WHISPER_MODEL_ROOT` (or `/models` in the container) before inference.

## Development and validation

From this directory:

```shell
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo clippy --all-targets --features fixture-probe --locked -- -D warnings
node --check web/app.js
```

The Tailwind stylesheet is checked in. After changing its source classes, regenerate it with `pnpm build:css` when Node and pnpm are installed. These checks do not measure runtime memory or establish transcription quality on every platform. Use `docker stats --no-stream subtitle-worker` and host GPU tools with representative media to measure the deployment you run.

The optional Docker `test` target includes the Rust `subtitle-studio-probe` executable. It expects the public whisper.cpp JFK speech fixture converted to `/library/sample.mkv` and an already downloaded `tiny-q5_1` model. Run it beside the server in an isolated container without network access; it checks actual transcription, SRT download and cached transcript reuse. It is excluded from the final production image. The recorded short CPU check and its limits are in [the memory budget guide](../docs/memory-budget.md).

For standalone SRT checks, run `subtitle-studio validate-srt FILE --source ORIGINAL.srt --video VIDEO.mkv` (the source/video options are optional). It returns a nonzero status for invalid structure or mismatched timing. Saved output selection also supports `validate-srt --job-id ID --target-language en`. Model inference quality and peak resource use require representative media. Cancellation stops native processing promptly; a translation/review request already sent to LM Studio can take until its response or `SUBTITLE_LLM_TIMEOUT` before cancellation completes.
