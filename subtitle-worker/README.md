# Subtitle Studio

Local browser UI for making timed `.srt` files from MKV, MP4, M4V, and WebM audio. FFmpeg extracts the selected audio track, Faster Whisper transcribes it with word timestamps, and a model served by LM Studio translates the cues into the selected output languages. Translations keep the source cue timings. The worker runs its own Python HTTP server and processing queue; n8n is independent.

## Start

Choose a runtime for your host. The checked-in Compose service uses an NVIDIA CUDA image and GPU reservation; the Python worker can also run directly on a CPU.

| Host | Example runtime |
| --- | --- |
| Windows with NVIDIA GPU | Docker Desktop using the WSL 2 backend, or native CPU mode below. |
| Linux with NVIDIA GPU | Docker Engine with the NVIDIA Container Toolkit, or native CPU mode below. |
| macOS (Intel or Apple silicon) | Native CPU mode below. Docker Desktop on macOS does not provide the CUDA GPU passthrough required by this Compose service. |

See [Docker Desktop GPU support](https://docs.docker.com/desktop/features/gpu/) and [NVIDIA Container Toolkit installation](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html) for host requirements. Apple GPU acceleration used by other model runtimes does not satisfy this service's NVIDIA CUDA requirement.

### Docker with NVIDIA on Windows or Linux

Set these values in the root `.env` (see `.env.example`). The paths shown here are Windows examples; choose existing directories on your own host:

```dotenv
SUBTITLE_LIBRARY_DIR=D:/homelab-media
SUBTITLE_MEDIA_DIR=D:/homelab-subtitles
WHISPER_MODEL=medium
WHISPER_LOCAL_FILES_ONLY=false
SUBTITLE_LLM_MODEL=
SUBTITLE_REVIEW_MODEL=
SUBTITLE_LLM_BASE_URL=http://host.docker.internal:1234/v1
SUBTITLE_LLM_TIMEOUT=600
```

For a Linux host, replace the two host paths with absolute Unix paths, for example:

```dotenv
SUBTITLE_LIBRARY_DIR=/srv/media
SUBTITLE_MEDIA_DIR=/srv/subtitles
```

Create the host directories and ensure Docker can read the media and write the export directory. Docker Desktop may require access to the selected Windows drive. `SUBTITLE_LIBRARY_DIR` should be the directory you want visible in the library, rather than an entire drive unless that is intended.

Run from the repository root. Docker commands in this guide have the same syntax in PowerShell, Bash, and zsh:

```shell
docker compose up -d --build subtitle-worker
```

If you use the repository's [portable path override](../README.md#platform-guide), include the same `-f docker-compose.yml -f compose.override.yaml` flags in every Docker command in this guide. Explicitly naming `subtitle-worker` starts that service even if an override assigns it to a disabled GPU profile; a profile does not convert it to CPU mode. Use native CPU setup when your host has no suitable NVIDIA GPU.

The service is defined in `compose/cloud-automation.yml`, included by the root `docker-compose.yml`. Its image includes FFmpeg, Python, CUDA libraries, and `faster-whisper==1.2.1`. Compose reserves one NVIDIA GPU, uses CUDA with `int8_float16`, and sets a 6 GB container memory limit. Use a Docker host with NVIDIA GPU support. Verify the container can see the GPU after it starts:

```shell
docker compose exec subtitle-worker python -c "import ctranslate2; print(ctranslate2.get_cuda_device_count())"
```

The command should print `1` for the configured single GPU. To confirm that Whisper itself loads on CUDA, run:

```shell
docker compose exec subtitle-worker python -c "import os; from faster_whisper import WhisperModel; WhisperModel(os.environ['WHISPER_MODEL'], device='cuda', compute_type='int8_float16', download_root=os.environ['HF_HUB_CACHE'], local_files_only=True); print('Whisper loaded on CUDA')"
```

Download the configured model first if it is not already cached (see the model-download command below). The first model load needs free GPU memory. Already cached transcripts will be reused, so choose a video without a cached transcript to measure transcription speed.

### Native CPU setup (Windows, macOS, and Linux)

Install a compatible Python interpreter and the `ffmpeg` and `ffprobe` executables, then make the command-line tools available on `PATH`. These examples select Python 3.11 on Windows/macOS and a Python 3.11 or 3.12 interpreter on Linux. The repository's unit tests run with Python 3.11; the pinned Faster Whisper release documents Python 3.9 or newer and `device="cpu", compute_type="int8"`. [Faster Whisper v1.2.1 requirements and usage](https://github.com/SYSTRAN/faster-whisper/blob/v1.2.1/README.md#requirements). Check the [CTranslate2 supported wheel platforms](https://opennmt.net/CTranslate2/installation.html) for your OS and architecture rather than assuming packages support the newest Python release.

| Host | Dependency example |
| --- | --- |
| Windows | Install Python 3.11 with its launcher (for example the [Python 3.11.9 Windows installer](https://www.python.org/downloads/release/python-3119/)) and a Windows build linked by [FFmpeg](https://ffmpeg.org/download.html#build-windows); add the FFmpeg `bin` directory to `PATH`. |
| macOS | With Homebrew installed, run `brew install python@3.11 ffmpeg`; see the [Python 3.11](https://formulae.brew.sh/formula/python@3.11) and [FFmpeg](https://formulae.brew.sh/formula/ffmpeg) formulae. |
| Debian 12 / Ubuntu 24.04 Linux | Run `sudo apt-get update`, then `sudo apt-get install python3 python3-venv python3-pip ffmpeg`. Their default interpreters are [Python 3.11](https://packages.debian.org/bookworm/python3) and [Python 3.12](https://packages.ubuntu.com/noble/python3), respectively. On other distributions, install Python 3.11 or 3.12 and equivalent FFmpeg/venv packages. |

Verify both command-line tools are reachable:

```shell
ffmpeg -version
ffprobe -version
```

The worker invokes FFmpeg and FFprobe directly, even though Faster Whisper's decoder bundles its own libraries. Native execution reads the process environment and does **not** load the root `.env`. `SUBTITLE_LIBRARY_DIR` and `SUBTITLE_MEDIA_DIR` are Compose host-mount settings; native execution needs the `*_ROOT` variables shown below.

These examples keep the virtual environment, library, outputs, job history, and model cache under a separate `subtitle-studio-example` folder in your home directory. Copy a supported video into its `library` folder, or replace `SUBTITLE_LIBRARY_ROOT` with your existing media folder. Start LM Studio's server on port 1234 if you want translations; source-only transcription can run without it. The native URL uses `127.0.0.1`, because the worker and LM Studio are on the same host.

From the repository root in **Windows PowerShell**:

```powershell
$subtitleExample = Join-Path $env:USERPROFILE 'subtitle-studio-example'
New-Item -ItemType Directory -Force -Path $subtitleExample | Out-Null
py -3.11 -m venv (Join-Path $subtitleExample 'venv')
$subtitlePython = Join-Path $subtitleExample 'venv/Scripts/python.exe'
& $subtitlePython -m pip install --upgrade pip
& $subtitlePython -m pip install -r subtitle-worker/requirements.txt

$env:SUBTITLE_LIBRARY_ROOT = Join-Path $subtitleExample 'library'
$env:SUBTITLE_OUTPUT_ROOT = Join-Path $subtitleExample 'output'
$env:SUBTITLE_DATA_ROOT = Join-Path $subtitleExample 'data'
$env:HF_HOME = Join-Path $subtitleExample 'models'
$env:HF_HUB_CACHE = Join-Path $env:HF_HOME 'hub'
$env:WHISPER_DEVICE = 'cpu'
$env:WHISPER_COMPUTE_TYPE = 'int8'
$env:WHISPER_MODEL = 'small'
$env:WHISPER_LOCAL_FILES_ONLY = 'false'
$env:SUBTITLE_LLM_BASE_URL = 'http://127.0.0.1:1234/v1'
$env:SUBTITLE_LLM_MODEL = ''
$env:SUBTITLE_REVIEW_MODEL = ''
$env:SUBTITLE_LLM_TIMEOUT = '600'
New-Item -ItemType Directory -Force -Path $env:SUBTITLE_LIBRARY_ROOT, $env:SUBTITLE_OUTPUT_ROOT, $env:SUBTITLE_DATA_ROOT, $env:HF_HUB_CACHE | Out-Null
& $subtitlePython subtitle-worker/worker.py
```

From the repository root in **macOS or Linux Bash/zsh**. On Linux, check that `python3 --version` reports Python 3.11 or 3.12; otherwise set `subtitle_bootstrap_python` to the compatible versioned executable you installed:

```bash
subtitle_example="$HOME/subtitle-studio-example"
mkdir -p "$subtitle_example"
subtitle_bootstrap_python=python3
if [ "$(uname -s)" = Darwin ]; then
  subtitle_bootstrap_python=python3.11
fi
"$subtitle_bootstrap_python" -m venv "$subtitle_example/venv"
subtitle_python="$subtitle_example/venv/bin/python"
"$subtitle_python" -m pip install --upgrade pip
"$subtitle_python" -m pip install -r subtitle-worker/requirements.txt

export SUBTITLE_LIBRARY_ROOT="$subtitle_example/library"
export SUBTITLE_OUTPUT_ROOT="$subtitle_example/output"
export SUBTITLE_DATA_ROOT="$subtitle_example/data"
export HF_HOME="$subtitle_example/models"
export HF_HUB_CACHE="$HF_HOME/hub"
export WHISPER_DEVICE=cpu
export WHISPER_COMPUTE_TYPE=int8
export WHISPER_MODEL=small
export WHISPER_LOCAL_FILES_ONLY=false
export SUBTITLE_LLM_BASE_URL=http://127.0.0.1:1234/v1
export SUBTITLE_LLM_MODEL=""
export SUBTITLE_REVIEW_MODEL=""
export SUBTITLE_LLM_TIMEOUT=600
mkdir -p "$SUBTITLE_LIBRARY_ROOT" "$SUBTITLE_OUTPUT_ROOT" "$SUBTITLE_DATA_ROOT" "$HF_HUB_CACHE"
"$subtitle_python" subtitle-worker/worker.py
```

The examples select `small` for an initial CPU run; choose another supported Whisper model in the UI as needed. No virtual-environment activation is required because commands call its Python executable directly. Stop the foreground server with Ctrl+C. Rerun the environment assignments in a new shell before restarting; keeping the same data and cache paths preserves saved work. These recipes are based on the worker's supported environment variables and upstream CPU support; live native transcription on all three platforms has not been tested for this documentation update.

### Use the UI and download models

Open <http://localhost:8099>. In Docker, the media library is a read-only mount of `SUBTITLE_LIBRARY_DIR`; in native mode, it is the directory set by `SUBTITLE_LIBRARY_ROOT`. Choose a video, an audio track, the source language (or auto), a Whisper transcription model, whether to save its transcript, and any target languages. Select an LM Studio translation model, then create the job. For a source-only SRT, clear the output-language selections and enable **Save original transcript**; this does not need LM Studio inference. The Jobs panel displays progress, the Whisper model used, and download links. Completed SRT files mirror the source media folder structure as `<video-stem>.<language>.srt` under `SUBTITLE_MEDIA_DIR/output` in Docker, or `SUBTITLE_OUTPUT_ROOT` in native mode.

The Python server listens on `0.0.0.0:8099` in both native and container modes, and the app has no authentication. The checked-in Compose mapping is `8099:8099`, which publishes it on all host interfaces. For Docker access limited to the host, use a local Compose override with `127.0.0.1:8099:8099` and recreate the service. Native mode also requires a host firewall rule or other network restriction because `SUBTITLE_BIND_ADDRESS` is not read by the current worker. See the repository [security guidance](../SECURITY.md) before exposing the UI; the worker can read mounted media and write subtitle data, so allow access only to trusted clients.

The UI serves a compiled Tailwind CSS file from the container and needs no browser access to a CDN. The generated `web/app.css` is checked in, so Docker builds do not need Node to build the UI. Building the image still requires access to the base image and Ubuntu/Python package sources unless those are cached. After changing classes in `web/index.html` or `web/app.js`, regenerate the CSS with the development commands below, include the updated CSS with your changes, then rebuild the container.

LM Studio's server must be reachable at `SUBTITLE_LLM_BASE_URL` with a model available for translation. The selector displays IDs returned by its `/v1/models` endpoint, excluding embedding and reranking models. If `SUBTITLE_LLM_MODEL` is set, that model is selected by default and added to the selector if the server does not list it; otherwise the first available model is selected. Whisper's CUDA settings come from Compose; native examples explicitly use CPU with `int8`. LM Studio controls its own model loading and GPU settings; the worker does not configure them. When both use the GPU, they can compete for GPU memory.

Model IDs containing `translategemma` use a separate adapter: it sends the language-specific translation template through LM Studio's raw `/v1/completions` endpoint and translates one timed cue per request. General instruction models use `/v1/chat/completions` in batches of eight cues. Both adapters validate their responses before saving translations. The unit suite covers request formats and responses with mocks; it does not establish live model quality or memory requirements.

The Whisper selector lists Faster Whisper models and marks the ones already downloaded in `HF_HUB_CACHE` (in the persistent `subtitle_models` volume for Docker). `WHISPER_MODEL` sets the default selection. Only one Whisper model is kept loaded at a time; switching models loads the selected one for the next uncached transcription. The first use downloads model files unless `WHISPER_LOCAL_FILES_ONLY=true`. To download a specific model in advance with Docker:

```shell
docker compose run --rm subtitle-worker python download_model.py large-v3
```

For native mode, keep the same cache environment variables and use `& $subtitlePython subtitle-worker/download_model.py large-v3` in PowerShell or `"$subtitle_python" subtitle-worker/download_model.py large-v3` in Bash/zsh from the repository root, after stopping the foreground server or setting up a second shell with the same environment.

With `WHISPER_LOCAL_FILES_ONLY=true`, download each model you intend to use before selecting it for a new transcription. Reload the page to refresh the downloaded labels. With a local LM Studio endpoint, audio, transcript text, and subtitles stay on your host and its local LM Studio server. Changing `SUBTITLE_LLM_BASE_URL` sends transcript and translation text to that configured server. Model downloads, if enabled, connect to Hugging Face. Models ending in `.en` support English audio only; choose a multilingual model for Japanese and other languages.

### Configuration and storage

The root `.env.example` documents the host-facing settings used by Compose. Host paths below are illustrative values for your `.env`; use existing directories appropriate to your OS:

| Setting | Example/value | Purpose |
| --- | --- | --- |
| `SUBTITLE_LIBRARY_DIR` | `D:/homelab-media` | Host media directory mounted read-only at `/library`. |
| `SUBTITLE_MEDIA_DIR` | `D:/homelab-subtitles` | Host directory mounted at `/media`; conventional SRT exports go to `/media/output`. |
| `WHISPER_MODEL` | `medium` | Default model selected for new jobs. |
| `WHISPER_LOCAL_FILES_ONLY` | `false` | When `true`, model loading requires a previously downloaded model. |
| `SUBTITLE_LLM_BASE_URL` | `http://host.docker.internal:1234/v1` | LM Studio endpoint, including its `/v1` prefix. |
| `SUBTITLE_LLM_MODEL` | empty | Default translation model ID; an empty value uses the first listed model. |
| `SUBTITLE_REVIEW_MODEL` | empty | Preferred reviewer; an empty value uses the automatic selection described below. |
| `SUBTITLE_LLM_TIMEOUT` | `600` | Timeout in seconds for each translation or review HTTP attempt. |

Compose fixes the container paths as `SUBTITLE_LIBRARY_ROOT=/library`, `SUBTITLE_OUTPUT_ROOT=/media/output`, and `SUBTITLE_DATA_ROOT=/data`. The `subtitle_models` named volume mounts at `/models`, with the model cache at `HF_HUB_CACHE=/models/hub`. The `subtitle_data` volume holds `/data/jobs.json`, transcript/translation/review caches, cached WAV audio, and `/data/job_outputs/<job_id>/` SRT snapshots. Ordinary service rebuilds and restarts preserve these volumes. Removing them, for example with `docker compose down -v`, removes the saved state and models.

Native mode stores the same job records and cache layout inside your configured `SUBTITLE_DATA_ROOT`, with exports in `SUBTITLE_OUTPUT_ROOT` and models in `HF_HUB_CACHE`. These directories are ordinary host folders rather than Docker volumes. Keep the same paths when restarting and back them up if you need to preserve jobs and generated files. The native examples do not alter the Compose service or its existing volumes.

## Reusing completed work

Every successful transcription saves the timed source transcript under `SUBTITLE_DATA_ROOT` (the persistent `subtitle_data` volume in Docker), even when **Save original transcript** is unchecked. That checkbox controls the source SRT export. If an output language matches the detected source language, the worker also exports the source SRT without translating it. Translation text is saved after each successful batch (one cue at a time for TranslateGemma). The extracted mono, 16 kHz WAV is cached under `SUBTITLE_DATA_ROOT/cache/audio` for retries; extraction is skipped entirely whenever a matching transcript is cached.

For example, make English subtitles first, then submit the same media and audio track with French selected. The second job reuses the source transcript, leaves the English translation alone, and translates only the French cues. Repeating an already completed language uses cached text. Its final quality review also reuses cached ratings when the reviewer and rubric match, so a fully cached job does not need new LM Studio inference. The Jobs panel shows which stages were reused. A failed translation can resume at its first missing batch on a new job. Each job's download is stored as a separate SRT snapshot, so a later export from another job cannot change that download. The conventional SRT under `SUBTITLE_OUTPUT_ROOT` reflects the latest export for that media and language.

Cache entries are specific to the media file, its size and modification time, the selected audio track and source-language setting, the Whisper model and transcription settings, the output language, and the LM Studio model. Changing any of these starts the affected stage again. The worker also samples the beginning and end of the media file to avoid reusing a cache for a same-size replacement. Jobs made before this cache feature have no stored source transcript; they need one new transcription before later languages can be added without Whisper. Cache files live under `SUBTITLE_DATA_ROOT/cache` (`/data/cache` in Docker).

## Status and troubleshooting

The UI receives job status through Server-Sent Events (SSE) at `/api/jobs/events`. Each connection starts with the latest jobs snapshot, then receives changes as they are saved. The stream sends a heartbeat every 15 seconds and the browser automatically reconnects after a disconnect, receiving a fresh snapshot. The Jobs panel shows the connection status. After 10 seconds without a working stream, it falls back to refreshing every 5 seconds; polling stops when live updates recover. Browsers without EventSource use the same fallback. The refresh button remains available.

Transcription progress is based on the last completed audio segment; translation progress is based on completed batches. A stalled LM Studio response can take up to `SUBTITLE_LLM_TIMEOUT` seconds per attempt. The worker retries a failed translation batch twice before marking the job failed; SRT files already produced remain downloadable. Jobs survive restarts in `SUBTITLE_DATA_ROOT`; interrupted steps can be retried from the Jobs panel. Only one generation, review, or retry task runs at a time.

The Jobs panel paginates the full saved history, with 5 jobs per page by default and options for 10 or 25. Status filters apply before pagination. Live updates preserve the current page and filter; changing the filter or page size returns to page 1. Previous and Next buttons and the job count remain visible at the bottom of the panel.

Each job starts collapsed, showing its name, status, current stage, and progress. Click its summary or use Enter/Space to expand downloads, processing steps, and reviews. Expansion is remembered during live updates and page/filter changes in the current browser session; refreshing the page collapses jobs again.

On screens wider than 1,024 pixels, the workspace fills the viewport with three panels. The library, subtitle options, and jobs scroll within their panels while the header remains visible. Narrower screens use **Library**, **Setup**, and **Jobs** tabs. Selecting a media file opens Setup, and submitting a job opens Jobs. Switching tabs preserves the selected media and settings.

The media library remembers the last successfully opened folder and selected video in this browser, restoring them after refreshing or reopening the app. The selected video is probed again to restore its details, audio track options, and library highlight. The browsed folder is remembered independently of the selected video's parent folder. If the saved folder was removed or is no longer readable, the app returns to the library root; an invalid saved video selection is cleared. Temporary connection or server failures keep the saved paths for retry. Browser storage must be enabled for persistence.

The SSE endpoint emits named `jobs` events. The web app requests `/api/jobs/events?page=1&page_size=5&status=all`; each event contains `jobs`, `page`, `page_size`, `total`, `total_all`, and `page_count`, matching the paginated `GET /api/jobs` response. Pages cover all saved jobs, with sizes from 1 to 50 and status values `all`, `completed`, `failed`, `running`, `queued`, or `cancelled`. Out-of-range pages are clamped to the last available page. Calls without pagination parameters retain the previous array response of the latest 100 jobs for existing API clients.

Event IDs identify the server session and saved revision. Reconnecting clients always receive current state, including after a server restart. If you place the app behind a reverse proxy, disable response buffering and allow idle connections to remain open beyond the heartbeat interval.

Progress bars animate between reported percentages and show moving stripes only while their job or step is running or cancelling. The animation does not advance the reported progress. Finished, failed, and cancelled work stays still. Refresh icons spin during a manual refresh, and newly displayed job cards have a brief entrance animation. These effects respect the operating system's reduced-motion setting and use the bundled offline CSS.

### Cancel and retry individual steps

Expand **Processing steps** on a job to see extraction, transcription, each requested translation, and any reviews. **Cancel** stops that step; **Cancel job** stops all remaining work for the job. Extraction can terminate FFmpeg immediately. Whisper and LM Studio cancellation takes effect at the next segment or request boundary, so the UI shows **Cancelling** while the current operation finishes. Cancelling a translation or review language allows the other languages to continue. Cancelling extraction or transcription blocks the dependent translations.

**Retry** is available for failed, cancelled, or blocked steps when their inputs exist. It queues only the selected step. Retrying transcription reuses a completed audio extraction; an interrupted transcription starts again. Once the transcript is available, retry the desired language steps. Translation retries reuse the saved or cached source transcript and completed translation batches without touching other languages. Review retries resume saved ratings. Completed SRTs remain downloadable, and retry activity is shown separately from the original generation result. Running, Queued, and Cancelled filters include these independent tasks.

The API accepts `POST /api/jobs/{job_id}/steps/cancel` and `/steps/retry` with `{"step":"transcription"}`, `{"step":"translation:fr"}`, or another displayed step key. `POST /api/jobs/{job_id}/cancel` with `{}` cancels the job's current work. The queue rejects duplicate work and ignores stale entries after cancellation and retry.

## Check subtitle quality

**Review translations** is optional and unchecked by default in the setup panel. Leave it unchecked to finish generation as soon as the requested SRTs are saved; you can review them later using **Review missing**. Checking it adds a final local LM Studio review of every translated language. API clients can send `review_enabled: false` to skip this stage; omitting the field preserves the previous automatic-review behavior.

The Jobs panel shows **Estimated fidelity** and **Fluency**, both on a 0–100 scale, plus review coverage and flagged cues. Expand a language's review to see the reviewer model and up to five of its lowest-rated cues, with the source text, translation, and explanation. Fidelity measures preservation of meaning against the Whisper transcript; fluency measures grammar and natural phrasing. The displayed scores are the arithmetic mean of the cue ratings. A cue is flagged when either rating is below 70.

These are model estimates, not a measured percentage of correct translations. The reviewer has no audio or trusted human translation, so an incorrect Whisper transcript can produce a misleadingly high fidelity score. Review also cannot establish correct timing or readability. Using the same model to translate and review can miss that model's own mistakes; the UI identifies this case. A different multilingual instruction model may provide a useful second opinion, but its ratings still need human verification.

Choose a **Translation review model** in the setup panel. **Automatic** uses `SUBTITLE_REVIEW_MODEL` if configured; otherwise it uses the translation model when that model supports general instructions, then another available instruction model. TranslateGemma is excluded from review because its adapter is designed for translation rather than scoring. To set an independent reviewer as the default in Docker, add its LM Studio model ID to `SUBTITLE_REVIEW_MODEL` in `.env` and recreate the service. In native mode, set that environment variable before restarting the worker. This remains a request to the same configured LM Studio server. A separate model may require more time and memory to load.

Review happens after all translated SRTs have been written, in batches of eight cues with adjacent context for sentences split across subtitles. No final score is shown until every cue has been reviewed. Review adds processing time, but successful batches are cached independently of transcription and translation. The cache includes the source transcript, target text and language, reviewer model ID, and review rubric version. Resubmitting the same media and settings reuses existing transcripts, translations, and reviews. Changing only the reviewer reruns only review. If review fails, the job completes with a review warning and its SRTs remain downloadable; use Review missing to resume the first missing review batch. Older jobs can receive scores through the same review-only controls described below.

### Review existing translations

Review requests use LM Studio's JSON schema output to constrain model responses. The worker also validates cue IDs, counts, and score ranges before saving each batch; malformed or incomplete ratings never produce a final score.

Use the reviewer selector in **Jobs & downloads** without selecting a video. **Review missing** at the top queues reviews for all eligible saved jobs, including jobs outside the current status filter or the latest 100 displayed jobs. A job's **Review missing** button reviews only its saved languages that do not have a completed review. This also works for a failed generation when some translated SRTs were already saved. Jobs without translated outputs, jobs currently generating subtitles, and jobs already waiting for or running a review are skipped with a reason.

Existing translations are reviewed against their saved source SRT, or the matching cached source transcript when that SRT was not exported. The original video need not be present if these saved texts are available. Source and target cues must have the same count and matching timestamps. Missing or mismatched data produces a clear message; a review never starts a new transcription or translation. The action reads the saved subtitle for that particular job, rather than a later export that may have overwritten the same media's conventional output filename.

**Re-review** requests a fresh evaluation and bypasses previously cached review ratings for the selected languages. Select a different local reviewer to get another model's opinion. A successful evaluation replaces the displayed score and refreshes the review cache. If it fails, the previous completed score and all SRT files remain available. **Retry review** resumes any successful batches from the interrupted evaluation. The job's original generation status and error are preserved, while review progress appears separately. Review tasks use the same queue as generation and wait for the current task to finish.

The local API supports `POST /api/jobs/{job_id}/review` with `review_model`, optional `languages`, and `force` (`true` for a fresh re-review); `POST /api/reviews/missing` with `review_model` queues missing reviews in bulk. These actions return queue status promptly and the UI receives progress through the job event stream. If the service restarts during review, its review task is marked interrupted; retry it to resume without changing the original generation result.

For Docker, run the built-in validator from the repository root with a job ID from `/api/jobs`:

```shell
docker compose exec subtitle-worker python validate_srt.py --job-id 2ced752ce1c9 --target-language en
```

For native mode, use the same root-path environment variables in the shell you configured above:

```powershell
& $subtitlePython subtitle-worker/validate_srt.py --job-id 2ced752ce1c9 --target-language en
```

```bash
"$subtitle_python" subtitle-worker/validate_srt.py --job-id 2ced752ce1c9 --target-language en
```

It checks SRT structure, chronological timestamps, video duration, and source/translation cue alignment when a source SRT was exported for that job. This command requires the video still to be present. Unlike saved-translation review, the validator does not retrieve source cues from the transcript cache. To check saved files directly, provide a target SRT with optional `--source` and `--video` paths; inside the container these paths must use the mounted container directories:

```shell
docker compose exec subtitle-worker python validate_srt.py /media/output/episode.en.srt --source /media/output/episode.ja.srt --video /library/episode.mkv --max-cps 20
```

For standalone native validation, pass host SRT/video paths to `subtitle-worker/validate_srt.py` using the appropriate virtual-environment Python executable. Quote paths containing spaces. Omit `--video` when the original video is unavailable, or `--source` when only checking the target file.

Structural issues cause exit code 1; readability flags alone leave exit code 0. The default flags cover more than two text lines, lines over 42 characters, durations under 0.8 or over 7 seconds, over 20 characters per second, and trailing hyphens. Alignment allows a 40 ms timing difference; video checks allow 500 ms after the reported duration.

The validator cannot prove the speech was recognized correctly or that a translation preserves the meaning. Play the video with each SRT and review samples near the start, middle, and end. Listen for missed or invented words in the source transcript; then compare the source and translated cues for names, negation, pronouns, omissions, and sentences split across cues. For a reliable accuracy score, compare against a trusted human transcript or translation.

### Thinking controls

The web app has separate **Translation thinking**, **Review thinking**, and **Saved review thinking** switches. Switchable models start with thinking off; each selection is remembered per model in this browser. The review switch is enabled only when review is selected. For saved reviews, select a specific reviewer to control thinking; Automatic leaves the model's own default in effect.

Support comes from LM Studio's `/api/v1/models` reasoning capabilities, including loaded instance IDs. Models that cannot switch between thinking and non-thinking, TranslateGemma, and servers without this metadata show a disabled switch and use the model default. The app sends `reasoning_effort: "none"` for off and the model's supported effort for on through `/v1/chat/completions`; binary `off`/`on` capabilities map to `none`/`high`. Explicit thinking overrides sent to the API for an unsupported model produce an error when inference is attempted. The app does not change LM Studio's global settings.

New generation requests accept `translation_thinking` and `review_thinking` as boolean or null. Saved review and bulk review requests accept `review_thinking`. Null or an omitted value preserves LM Studio's default and the existing cache identity. Explicit on/off values have separate translation and review caches; changing them reuses audio and transcription but computes new results for that mode. Step retries retain the mode of the original generation or review task. Job cards and completed reports record the selected mode; old results whose mode was not specified retain their previous metadata.

Thinking can add latency and consume output/context tokens; it does not guarantee better subtitles. Configure context capacity in LM Studio to fit the source cues, review context, and response for your selected model. Review scores remain model estimates.

## Local API

All routes use the same host and port as the UI. Library paths are relative to `SUBTITLE_LIBRARY_ROOT` (mounted from `SUBTITLE_LIBRARY_DIR` in Docker) and use forward slashes on every OS, such as `shows/episode.mkv`; absolute paths, backslashes, and paths outside the library are rejected. Use the stream `index` from `/api/media` as `audio_stream_index`, rather than the track's position in a list.

| Method | Route | Purpose |
| --- | --- | --- |
| GET | `/api/config` | Language codes, Whisper models/download status, LM Studio models/defaults, thinking capabilities, and connection errors. |
| GET | `/api/library?path=shows` | Folders and supported video files; an empty path browses the library root. |
| GET | `/api/media?path=shows/episode.mkv` | Duration and available audio streams, including language, codec, title, channels, and offset. |
| GET | `/api/jobs` | Latest 100 jobs as an array, or the paginated response described above when view parameters are supplied. |
| GET | `/api/jobs/events` | SSE snapshots; accepts the same view parameters as `/api/jobs`. |
| GET | `/api/jobs/{job_id}` | One job, outputs, quality reports, review availability, and processing-step controls. |
| GET | `/api/jobs/{job_id}/files/{language}` | Download the job's saved SRT. |
| POST | `/api/jobs` | Queue subtitle generation. |
| POST | `/api/jobs/{job_id}/cancel` | Cancel current work for the job; send `{}`. |
| POST | `/api/jobs/{job_id}/steps/cancel` | Cancel a displayed step; send its `step` key. |
| POST | `/api/jobs/{job_id}/steps/retry` | Queue a retry of an eligible step; send its `step` key. |
| POST | `/api/jobs/{job_id}/review` | Review saved translations, optionally selecting `languages` or setting `force: true`. |
| POST | `/api/reviews/missing` | Queue missing reviews across all eligible jobs; returns `queued` IDs and `skipped` IDs with reasons. |

For example, after discovering the media path and audio stream index, send this JSON to `POST /api/jobs` with `Content-Type: application/json`:

```json
{
  "path": "shows/episode.mkv",
  "audio_stream_index": 1,
  "source_language": "auto",
  "whisper_model": "medium",
  "transcript": true,
  "targets": ["en", "fr"],
  "model": "your-lm-studio-model-id",
  "review_enabled": false,
  "review_model": "",
  "translation_thinking": null,
  "review_thinking": null
}
```

Only `path`, a valid `audio_stream_index`, and at least one output (`transcript: true` or nonempty `targets`) are required. Omitted `source_language` uses `auto`, `whisper_model` uses `WHISPER_MODEL`, `model` uses automatic selection, and `transcript` defaults to `false`. API generation defaults `review_enabled` to `true`, while the browser sends `false` until **Review translations** is checked. Supported manual source and target codes are returned by `/api/config`. Review routes accept `review_model` and `review_thinking`; only the single-job review route accepts `languages` and `force: true`.

Successful POST actions return HTTP 202 with queue state. Invalid input returns JSON with an `error` and HTTP 400, missing jobs return 404, and conflicting work for the same job returns 409. A 202 response means the task was accepted, not that transcription or inference succeeded; inspect the job or SSE stream for the result. POST bodies must be JSON objects between 1 and 16,384 bytes.

## Security audits

The repository [security audit workflow](../.github/workflows/security-audit.yml) scans this worker's Python sources with Bandit and audits its resolved Python dependencies. It also checks the pnpm lockfile, secret history, privacy rules, workflow syntax, and Dockerfiles. See [SECURITY.md](../SECURITY.md) for audit coverage, current baseline findings, and reporting guidance. Do not put deployment addresses, credentials, or private media paths in this guide or other tracked documentation.

## Development and checks

From `subtitle-worker`, regenerate the bundled stylesheet using the checked-in pnpm lockfile:

```shell
pnpm install --frozen-lockfile
pnpm build:css
```

`pnpm watch:css` rebuilds while editing. The package has CSS build/watch scripts only; the frontend tests use Node's built-in test runner. Include `web/app.css` whenever changes to `web/input.css`, `web/index.html`, or `web/app.js` affect styles.

The worker image uses `subtitle-worker/` as its build context and its `.dockerignore` is a strict allowlist: the Dockerfile, `requirements.txt`, `worker.py`, `download_model.py`, `validate_srt.py`, and the five current files in `web/` (`app.css`, `app.js`, `favicon.svg`, `index.html`, and `input.css`). Files such as tests, README content, Python caches, and local environments stay out of the build context. If the image later needs another source or web asset, add that file explicitly to the allowlist and check the Dockerfile copy steps; broadening the rule to all of `web/` would also send unreviewed files into the build context.

From the repository root, run the Python suite using **Windows PowerShell**:

```powershell
python -m unittest discover -s subtitle-worker -p "test_*.py"
```

Or using **macOS/Linux Bash/zsh**:

```bash
python3 -m unittest discover -s subtitle-worker -p "test_*.py"
```

The Node test command is the same on every OS:

```shell
node --test subtitle-worker/test_animation_frontend.cjs subtitle-worker/test_job_disclosure_frontend.cjs subtitle-worker/test_sse_frontend.cjs subtitle-worker/test_workspace_frontend.cjs
```

The Python tests cover library containment, timing, model adapters, cache reuse, review validation, saved-review actions, thinking, cancel/retry behavior, HTTP routes, pagination, and SSE. The Node tests use controlled DOM/event fixtures for workspace tabs, job disclosure, progress animation, pagination, and SSE/polling recovery. They require Python and Node, but no live GPU, model download, LM Studio server, or Node dependencies. These checks do not replace a live media-generation test and playback review.

For Docker logs:

```shell
docker compose logs -f subtitle-worker
```

Native logs are printed in the foreground terminal. When changing the UI or worker code, rebuild the Docker service with the startup command above, or restart the native worker with the same environment. Regenerate CSS first if styles changed. The existing `n8n` service is independent and remains available for other automations.
