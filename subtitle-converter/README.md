# Automatic Subtitle Converter

`subtitle-converter` is an opt-in watcher that creates UTF-8 SubRip (`.srt`) sidecars from embedded subtitle tracks and supported subtitle files. It uses the official [Subtitle Edit command-line converter](https://github.com/SubtitleEdit/subtitleedit/releases) and Tesseract when an input track contains images. The container is built locally from the pinned converter release.

## Start the service

Set the four `CONVERTER_*_DIR` values in the ignored root `.env` to the host folders used for your movie and TV libraries. The `.env.example` values are examples for the current Compose layout; replace them with paths on your machine. On Windows with Docker Desktop, use Windows drive paths shared with Docker Desktop. On macOS and Linux, use absolute host paths such as `/Volumes/media/movies` or `/srv/media/movies`.

Start the opt-in service from the repository root:

```shell
docker compose up -d --build subtitle-converter
```

The build downloads Subtitle Edit's Linux `seconv` v5.2.0 release and checks its SHA-256 digest for `amd64` or `arm64`. Docker needs network access while building; the running converter uses `network_mode: none`. It has no published ports, GPU reservation, or Docker socket. The process runs as UID/GID 1000 with a read-only container root; it writes generated SRTs into the mounted libraries and its scan state into the persistent `subtitle_converter_data` volume.

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

The mounted library directories must be writable by container UID/GID 1000 so the converter can create SRT sidecars. Keep the host paths and container mounts aligned with Bazarr and your media manager roots. To use different folders, change only the applicable `CONVERTER_*_DIR` values in your private `.env`; do not put private deployment paths in shared documentation.

## Run once or preview

The container runs repeated scans by default. The script supports `--once` to scan and convert once, or `--dry-run` to list inputs that would be processed without writing files. For native use, install Python 3, Subtitle Edit's `seconv` v5.2.0, FFmpeg/`ffprobe`, MKVToolNix/`mkvmerge`, Tesseract with the needed trained data, and (for full language metadata mapping) the `iso-codes` data package. Then run the script with one or more `--root` options for actual library paths. The script itself uses only the Python standard library. See the [Subtitle Edit releases](https://github.com/SubtitleEdit/subtitleedit/releases), [Tesseract documentation](https://tesseract-ocr.github.io/), [FFmpeg downloads](https://ffmpeg.org/download.html), and [MKVToolNix downloads](https://mkvtoolnix.download/downloads.html) for platform-specific installers.

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
python -m unittest discover -s subtitle-converter -p test_converter.py
```

The security audit workflow also builds the pinned converter image and runs its integration fixtures with networking disabled and no host media mounted. Those fixtures cover text extraction and local OCR; they do not verify every media codec, language, or platform filesystem.

Set `CONVERTER_SECONV`, `CONVERTER_FFPROBE`, and `CONVERTER_MKVMERGE` if the tools are not available on `PATH`. Native deployments should also set `CONVERTER_OCR_LANGUAGE` for unknown-language image subtitles. The native process must be able to create temporary files next to each input so completed SRTs can be published without overwriting existing files.
