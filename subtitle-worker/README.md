# MKV audio to English SRT

The path-input n8n workflow starts a job for an MKV file in a mounted source
folder. A private worker extracts the first audio track with FFmpeg, translates
speech to English with local Faster Whisper, edits the English with LM Studio,
and writes a timed `.srt` file. A second workflow shows job progress. The video
is never uploaded to n8n or a cloud API.

## Setup

1. Make `D:\subtitles\input` and `D:\subtitles\output`, or set
   `SUBTITLE_MEDIA_DIR` in the root `.env` to another host directory. Set
   `SUBTITLE_SOURCE_DIR` to the host folder containing your MKVs. The default is
   `D:/subtitles/input`; the worker mounts this folder read-only as `/source`.
   You can use `SUBTITLE_SOURCE_DIR=D:/film` for films or `D:/` for any file on
   the D: drive. Create the selected host folder before starting Docker Compose.
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
4. In n8n, import [`n8n-workflow.json`](n8n-workflow.json) and
   [`n8n-progress-workflow.json`](n8n-progress-workflow.json). Publish both.
   Their forms require a logged-in n8n user with execute permission.
5. Open the first workflow's production form URL and enter an MKV file path,
   for example `D:\subtitles\input\Movie.mkv` or `/source/Movie.mkv`. The file
   must be inside `SUBTITLE_SOURCE_DIR`. The form returns a job ID. Open the
   progress workflow's form URL, enter that ID, and submit it again whenever you
   want an updated status. The output appears at
   `D:\subtitles\output\<name>.en.srt` by default.

The original automatic folder scanner is still available as
[`n8n-folder-scan.json`](n8n-folder-scan.json). It checks `input/` every minute
and queues files only after their size stays unchanged across scans and their
last modification is at least 90 seconds old. Its first scan may therefore
show `submitted: []`. Use the path form to submit a completed file immediately.

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
- `SUBTITLE_SOURCE_DIR` controls which host paths the path-input form may use.
- `SUBTITLE_LLM_BASE_URL` points to your LM Studio OpenAI-compatible endpoint.
- `SUBTITLE_LLM_MODEL` selects the proofreading model explicitly.

Only one video is processed at a time. The progress form shows `queued`,
`extracting_audio`, `loading_speech_model`, `transcribing`, `proofreading`,
`writing_srt`, `completed`, or `failed`. Percentages are estimates: during
transcription they use the latest subtitle timestamp divided by audio duration.
Model download and audio extraction can stay at one percentage for a while.
You can also inspect logs with `docker compose logs -f subtitle-worker`.
To inspect all jobs from the host, run:

```powershell
docker compose exec subtitle-worker python -c "import urllib.request; print(urllib.request.urlopen('http://localhost:8099/jobs').read().decode())"
```

If a job fails, the worker writes
`output/<name>.en.error.txt` with the error. Fix the issue, remove that error
file, and resubmit the path or let the next stable-file scan retry. Existing
`.en.srt` files are not overwritten; remove an output file to reprocess its MKV.

Rebuilding or recreating the worker interrupts any active job and clears its
in-memory job IDs. Let an active translation finish before running Compose
again, then import the updated forms. Completed SRT files remain on disk.

Speech recognition and machine translation can mishear names, music, overlapping
speakers, or difficult accents. Review the result against the video before
publishing it. The workflow preserves model timestamps, but no automatic model
can guarantee frame-perfect synchronization or flawless grammar for every file.
