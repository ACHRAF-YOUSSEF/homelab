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

Open <http://localhost:8099>. The media library is a read-only mount of `SUBTITLE_LIBRARY_DIR`. Choose a video, an audio track, the source language (or auto), whether to save its transcript, and any target languages. Select a model loaded in LM Studio, then create the job. The Jobs panel displays progress and download links. Completed SRT files are also saved under `SUBTITLE_MEDIA_DIR/output`, mirroring the source media folder structure. The UI only binds to localhost by default. For access from another device on your LAN, set `SUBTITLE_BIND_ADDRESS` to your computer's LAN IP address and restart this service.

LM Studio's local server must be running with an instruction model available for translation. The selector displays IDs returned by its `/v1/models` endpoint; with just-in-time loading enabled, that endpoint can list downloaded models that are not yet in memory. If `SUBTITLE_LLM_MODEL` is set, that model is selected by default; otherwise the first available model is selected. Whisper uses CPU by default, while LM Studio may use your GPU. For a 12 GB GPU, Gemma 4 12B Instruct Q4_K_M is a practical general model. Larger models that spill into system RAM can be substantially slower.

TranslateGemma 12B is supported through a separate adapter: it uses Google's language-specific template through LM Studio's raw `/v1/completions` endpoint and translates one timed cue per request. This preserves SRT boundaries but may take longer than the eight-cue batches used for general models. On a 12 GB GPU, try Q5_K_M with a 2,048-token context for quality; use Q4_K_M if GPU memory is tight. Q8_0 exceeds 12 GB. The adapter has unit coverage, but it needs a live test once you download and load TranslateGemma in LM Studio.

The first Whisper run downloads model files into the persistent `subtitle_models` volume unless `WHISPER_LOCAL_FILES_ONLY=true`. To download them in advance:

```powershell
docker compose run --rm subtitle-worker python download_model.py
```

Then set `WHISPER_LOCAL_FILES_ONLY=true` to prevent model downloads during jobs. Audio, transcript text, and subtitles remain on your machine. Model downloads, if enabled, connect to Hugging Face.

## Reusing completed work

Every job saves the timed source transcript to the persistent `subtitle_data` volume, even when **Save original transcript** is unchecked. Translation text is saved after each successful batch (one cue at a time for TranslateGemma). The extracted WAV is temporary and is skipped entirely whenever a matching transcript is cached.

For example, make English subtitles first, then submit the same media and audio track with French selected. The second job reuses the source transcript, leaves the English translation alone, and translates only the French cues. Repeating an already completed language uses cached text and does not call LM Studio. The Jobs panel shows which stages were reused. A failed translation can resume at its first missing batch on a new job. Each job's download is stored as a small immutable SRT snapshot, so a later export for another audio track or model cannot change an earlier download. The conventional SRT under `SUBTITLE_MEDIA_DIR/output` reflects the latest job for that media and language.

Cache entries are specific to the media file, its size and modification time, the selected audio track and source-language setting, the Whisper model and transcription settings, the output language, and the LM Studio model. Changing any of these starts the affected stage again. The worker also samples the beginning and end of the media file to avoid reusing a cache for a same-size replacement. Jobs made before this cache feature have no stored source transcript; they need one new transcription before later languages can be added without Whisper. Cache files live in the `subtitle_data` Docker volume under `/data/cache`.

## Status and troubleshooting

The UI polls job status every 2.5 seconds. Transcription progress is based on the last completed audio segment; translation progress is based on completed batches. A stalled LM Studio response can take up to `SUBTITLE_LLM_TIMEOUT` seconds per attempt. The worker retries a failed translation batch twice before marking the job failed; SRT files already produced remain downloadable. Jobs survive service restarts in `subtitle_data`, although an in-progress job is marked interrupted and must be submitted again. Only one job runs at a time.

## Check subtitle quality

After rebuilding the container, run the built-in validator from the repository root with a job ID from `/api/jobs`:

```powershell
docker compose exec subtitle-worker python validate_srt.py --job-id 2ced752ce1c9 --target-language en
```

It checks SRT structure, chronological timestamps, video duration, source/translation cue alignment, and flags subtitles that may be too brief, too long, or too dense to read. It cannot prove the speech was recognized correctly or that a translation preserves the meaning. Play the video with each SRT and review samples near the start, middle, and end. Listen for missed or invented words in the source transcript; then compare the source and translated cues for names, negation, pronouns, omissions, and sentences split across cues. For a reliable accuracy score, compare against a trusted human transcript or translation. The current Compose configuration runs Whisper on CPU with `int8`; LM Studio controls GPU use for translation separately.

For logs:

```powershell
docker compose logs -f subtitle-worker
```

When changing Compose or the UI code, rebuild the service with the startup command above. The existing `n8n` service is independent and remains available for other automations.
