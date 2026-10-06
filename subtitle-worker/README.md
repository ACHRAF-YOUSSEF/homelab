# Subtitle Studio

Private local browser UI for making timed `.srt` files from MKV, MP4, M4V, and WebM audio. Faster Whisper transcribes the selected track with word timestamps. A model running in LM Studio translates each cue into the selected output languages. There is no n8n workflow or external transcription API.

## Start

Set these values in the repo `.env` (see `.env.example`):

```dotenv
SUBTITLE_LIBRARY_DIR=D:/
SUBTITLE_MEDIA_DIR=D:/subtitles
SUBTITLE_BIND_ADDRESS=127.0.0.1
WHISPER_MODEL=medium
SUBTITLE_LLM_BASE_URL=http://host.docker.internal:1234/v1
SUBTITLE_LLM_TIMEOUT=600
```

Run from the repository root:

```powershell
docker compose up -d --build subtitle-worker
```

Docker Desktop on Windows needs the WSL 2 backend with NVIDIA GPU support. Verify the container can see the GPU after it starts:

```powershell
docker compose exec subtitle-worker python -c "import ctranslate2; print(ctranslate2.get_cuda_device_count())"
```

The command should print `1` for the configured single GPU. To confirm that Whisper itself loads on CUDA, run:

```powershell
docker compose exec subtitle-worker python -c "from faster_whisper import WhisperModel; WhisperModel('medium', device='cuda', compute_type='int8_float16', download_root='/models', local_files_only=True); print('Whisper loaded on CUDA')"
```

The first model load needs free GPU memory. Already cached transcripts will be reused, so choose a video without a cached transcript to measure transcription speed.

Open <http://localhost:8099>. The media library is a read-only mount of `SUBTITLE_LIBRARY_DIR`. Choose a video, an audio track, the source language (or auto), a Whisper transcription model, whether to save its transcript, and any target languages. Select a model loaded in LM Studio, then create the job. The Jobs panel displays progress, the Whisper model used, and download links. Completed SRT files are also saved under `SUBTITLE_MEDIA_DIR/output`, mirroring the source media folder structure. The UI only binds to localhost by default. For access from another device on your LAN, set `SUBTITLE_BIND_ADDRESS` to your computer's LAN IP address and restart this service.

The UI serves a compiled Tailwind CSS file from the container. It needs no browser access to a CDN. The generated `web/app.css` is included in the project, so Docker builds do not need Node or internet access. After changing classes in `web/index.html` or `web/app.js`, regenerate it from the `subtitle-worker` directory with `pnpm install --frozen-lockfile` and `pnpm build:css`, include the updated CSS with your changes, then rebuild the container.

LM Studio's local server must be running with an instruction model available for translation. The selector displays IDs returned by its `/v1/models` endpoint; with just-in-time loading enabled, that endpoint can list downloaded models that are not yet in memory. If `SUBTITLE_LLM_MODEL` is set, that model is selected by default; otherwise the first available model is selected. Whisper runs on the NVIDIA GPU with `int8_float16`; LM Studio also uses the GPU for translation. On a 12 GB GPU, unload large LM Studio models before starting a new transcription if VRAM is tight. For translation, a 12B Q4_K_M model is a practical general choice; larger models that spill into system RAM can be substantially slower.

TranslateGemma 12B is supported through a separate adapter: it uses Google's language-specific template through LM Studio's raw `/v1/completions` endpoint and translates one timed cue per request. This preserves SRT boundaries but may take longer than the eight-cue batches used for general models. On a 12 GB GPU, try Q5_K_M with a 2,048-token context for quality; use Q4_K_M if GPU memory is tight. Q8_0 exceeds 12 GB. The adapter has unit coverage, but it needs a live test once you download and load TranslateGemma in LM Studio.

The Whisper selector lists Faster Whisper models and marks the ones already downloaded in the persistent `subtitle_models` volume. `WHISPER_MODEL` sets the default selection. Only one Whisper model remains loaded in GPU memory at a time; switching models loads the selected one for the next uncached transcription. The first use downloads model files unless `WHISPER_LOCAL_FILES_ONLY=true`. To download a specific model in advance:

```powershell
docker compose run --rm subtitle-worker python download_model.py large-v3
```

With `WHISPER_LOCAL_FILES_ONLY=true`, download each model you intend to use before selecting it for a new transcription. Reload the page to refresh the downloaded labels. Audio, transcript text, and subtitles remain on your machine. Model downloads, if enabled, connect to Hugging Face. Models ending in `.en` support English audio only; choose a multilingual model for Japanese and other languages.

## Reusing completed work

Every job saves the timed source transcript to the persistent `subtitle_data` volume, even when **Save original transcript** is unchecked. Translation text is saved after each successful batch (one cue at a time for TranslateGemma). The extracted WAV is temporary and is skipped entirely whenever a matching transcript is cached.

For example, make English subtitles first, then submit the same media and audio track with French selected. The second job reuses the source transcript, leaves the English translation alone, and translates only the French cues. Repeating an already completed language uses cached text. Its final quality review also reuses cached ratings when the reviewer and rubric match, so a fully cached job does not need new LM Studio inference. The Jobs panel shows which stages were reused. A failed translation can resume at its first missing batch on a new job. Each job's download is stored as a small immutable SRT snapshot, so a later export for another audio track or model cannot change an earlier download. The conventional SRT under `SUBTITLE_MEDIA_DIR/output` reflects the latest job for that media and language.

Cache entries are specific to the media file, its size and modification time, the selected audio track and source-language setting, the Whisper model and transcription settings, the output language, and the LM Studio model. Changing any of these starts the affected stage again. The worker also samples the beginning and end of the media file to avoid reusing a cache for a same-size replacement. Jobs made before this cache feature have no stored source transcript; they need one new transcription before later languages can be added without Whisper. Cache files live in the `subtitle_data` Docker volume under `/data/cache`.

## Status and troubleshooting

The UI receives job status through Server-Sent Events (SSE) at `/api/jobs/events`. Each connection starts with the latest jobs snapshot, then receives changes as they are saved. The stream sends a heartbeat every 15 seconds and the browser automatically reconnects after a disconnect, receiving a fresh snapshot. The Jobs panel shows the connection status. After 10 seconds without a working stream, it falls back to refreshing every 5 seconds; polling stops when live updates recover. Browsers without EventSource use the same fallback. The refresh button remains available.

Transcription progress is based on the last completed audio segment; translation progress is based on completed batches. A stalled LM Studio response can take up to `SUBTITLE_LLM_TIMEOUT` seconds per attempt. The worker retries a failed translation batch twice before marking the job failed; SRT files already produced remain downloadable. Jobs survive service restarts in `subtitle_data`; interrupted steps can be retried from the Jobs panel. Only one generation, review, or retry task runs at a time.

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

Choose a **Translation review model** in the setup panel. **Automatic** uses `SUBTITLE_REVIEW_MODEL` if configured; otherwise it uses the translation model when that model supports general instructions, then another available instruction model. TranslateGemma is excluded from review because its adapter is designed for translation rather than scoring. To set an independent reviewer as the default, add its LM Studio model ID to `SUBTITLE_REVIEW_MODEL` in `.env` and recreate the service. This remains a local request to the same LM Studio server. A separate model may require more time and memory to load.

Review happens after all translated SRTs have been written, in batches of eight cues with adjacent context for sentences split across subtitles. No final score is shown until every cue has been reviewed. Review adds processing time, but successful batches are cached independently of transcription and translation. The cache includes the source transcript, target text and language, reviewer model ID, and review rubric version. Resubmitting the same media and settings reuses existing transcripts, translations, and reviews. Changing only the reviewer reruns only review. If review fails, the job completes with a review warning and its SRTs remain downloadable; use Review missing to resume the first missing review batch. Older jobs can receive scores through the same review-only controls described below.

### Review existing translations

Review requests use LM Studio's JSON schema output to constrain model responses. The worker also validates cue IDs, counts, and score ranges before saving each batch; malformed or incomplete ratings never produce a final score.

Use the reviewer selector in **Jobs & downloads** without selecting a video. **Review missing** at the top queues reviews for all eligible saved jobs, including jobs outside the current status filter or the latest 100 displayed jobs. A job's **Review missing** button reviews only its saved languages that do not have a completed review. This also works for a failed generation when some translated SRTs were already saved. Jobs without translated outputs, jobs currently generating subtitles, and jobs already waiting for or running a review are skipped with a reason.

Existing translations are reviewed against their saved source SRT, or the matching cached source transcript when that SRT was not exported. The original video need not be present if these saved texts are available. Source and target cues must have the same count and matching timestamps. Missing or mismatched data produces a clear message; a review never starts a new transcription or translation. The action reads the saved subtitle for that particular job, rather than a later export that may have overwritten the same media's conventional output filename.

**Re-review** requests a fresh evaluation and bypasses previously cached review ratings for the selected languages. Select a different local reviewer to get another model's opinion. A successful evaluation replaces the displayed score and refreshes the review cache. If it fails, the previous completed score and all SRT files remain available. **Retry review** resumes any successful batches from the interrupted evaluation. The job's original generation status and error are preserved, while review progress appears separately. Review tasks use the same queue as generation and wait for the current task to finish.

The local API supports `POST /api/jobs/{job_id}/review` with `review_model`, optional `languages`, and `force` (`true` for a fresh re-review); `POST /api/reviews/missing` with `review_model` queues missing reviews in bulk. These actions return queue status promptly and the UI receives progress through the job event stream. If the service restarts during review, its review task is marked interrupted; retry it to resume without changing the original generation result.

After rebuilding the container, run the built-in validator from the repository root with a job ID from `/api/jobs`:

```powershell
docker compose exec subtitle-worker python validate_srt.py --job-id 2ced752ce1c9 --target-language en
```

It checks SRT structure, chronological timestamps, video duration, source/translation cue alignment, and flags subtitles that may be too brief, too long, or too dense to read. It cannot prove the speech was recognized correctly or that a translation preserves the meaning. Play the video with each SRT and review samples near the start, middle, and end. Listen for missed or invented words in the source transcript; then compare the source and translated cues for names, negation, pronouns, omissions, and sentences split across cues. For a reliable accuracy score, compare against a trusted human transcript or translation. The current Compose configuration runs Whisper on the NVIDIA GPU with `int8_float16`; LM Studio controls GPU use for translation separately.

### Thinking controls

The web app has separate **Translation thinking**, **Review thinking**, and **Saved review thinking** switches. Switchable models start with thinking off; each selection is remembered per model in this browser. The review switch is enabled only when review is selected. For saved reviews, select a specific reviewer to control thinking; Automatic leaves the model's own default in effect.

Support comes from LM Studio's `/api/v1/models` reasoning capabilities, including loaded instance IDs. Models that cannot switch between thinking and non-thinking, TranslateGemma, and servers without this metadata show a disabled switch and use the model default. The app sends `reasoning_effort: "none"` for off and the model's supported effort for on through `/v1/chat/completions`. These values were verified against local Gemma 4 E4B with structured JSON output; the app does not change LM Studio's global settings.

New generation requests accept `translation_thinking` and `review_thinking` as boolean or null. Saved review and bulk review requests accept `review_thinking`. Null or an omitted value preserves LM Studio's default and the existing cache identity. Explicit on/off values have separate translation and review caches; changing them reuses audio and transcription but computes new results for that mode. Step retries retain the mode of the original generation or review task. Job cards and completed reports record the selected mode; old results whose mode was not specified retain their previous metadata.

Thinking can add latency and consume output/context tokens; it does not guarantee better subtitles. Keep an 8,192-token context for typical non-thinking batches and allow additional context space when reasoning is enabled. Review scores remain model estimates.

For logs:

```powershell
docker compose logs -f subtitle-worker
```

When changing Compose or the UI code, rebuild the service with the startup command above. The existing `n8n` service is independent and remains available for other automations.
