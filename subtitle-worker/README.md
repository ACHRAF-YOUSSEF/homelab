# MKV audio to English SRT

This n8n workflow watches a shared input folder for `.mkv` files. A private worker
extracts the first audio track with FFmpeg, translates speech to English with a
local Faster Whisper model, edits the English with the LM Studio model already
used by this homelab, and writes a timed `.srt` file. The video is never sent to
n8n or a cloud API.

## Setup

1. Make `D:\subtitles\input` and `D:\subtitles\output`, or set
   `SUBTITLE_MEDIA_DIR` in the root `.env` to another host directory. The worker
   creates the subdirectories if needed, but the host directory should exist
   before starting Docker Compose.
2. In LM Studio, load a multilingual instruction model that translates well and
   start its OpenAI-compatible server on port `1234`. Make the server reachable
   from Docker at `host.docker.internal`. Set `SUBTITLE_LLM_MODEL` to the loaded
   model ID if LM Studio exposes more than one. The worker uses the first ID
   returned by `/v1/models` when this setting is blank.
3. Run `docker compose up -d --build subtitle-worker n8n` from the repository
   root. The first translation downloads the Faster Whisper model into the
   persistent `subtitle_models` volume. The default `medium` model runs on CPU;
   a long movie can take a long time. `WHISPER_MODEL=small` is faster with lower
   recognition quality. `WHISPER_MODEL=large-v3` is better but needs more memory
   and is much slower on CPU.
4. In n8n, choose **Import from File** and select
   [`n8n-workflow.json`](n8n-workflow.json). Save and activate the workflow.
5. Copy a completed MKV into `D:\subtitles\input`. Once its size is unchanged
   across scans and its last modification is at least 90 seconds old, the worker
   queues it. The result appears as `D:\subtitles\output\<name>.en.srt`.

The output is UTF-8 SRT. FFmpeg preserves initial audio silence so timestamps
remain relative to the video start. Whisper supplies segment and word timestamps;
LM Studio changes only the cue text. The worker never rewrites cue times after
proofreading.

## Options and troubleshooting

The defaults are in [`compose/cloud-automation.yml`](../compose/cloud-automation.yml)
and tunable values are listed in [`.env.example`](../.env.example).

- `WHISPER_LANGUAGE=ja` gives Whisper a Japanese language hint. Leave it blank
  for automatic detection.
- `SUBTITLE_AUDIO_TRACK=0` selects the first audio track. Use `1` for the second,
  and so on. This is an index among audio tracks, not the global MKV stream ID.
- `SUBTITLE_MIN_AGE_SECONDS=90` controls the minimum time since the file changed.
- `SUBTITLE_LLM_BASE_URL` points to your LM Studio OpenAI-compatible endpoint.
- `SUBTITLE_LLM_MODEL` selects the proofreading model explicitly.

Only one video is processed at a time. Inspect progress with
`docker compose logs -f subtitle-worker`. If a job fails, the worker writes
`output/<name>.en.error.txt` with the error. Fix the issue, remove that error
file, and the next stable-file scan will retry. Existing `.en.srt` files are not
overwritten; remove an output file to reprocess its MKV.

Speech recognition and machine translation can mishear names, music, overlapping
speakers, or difficult accents. Review the result against the video before
publishing it. The workflow preserves model timestamps, but no automatic model
can guarantee frame-perfect synchronization or flawless grammar for every file.
