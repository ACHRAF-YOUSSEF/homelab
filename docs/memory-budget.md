# Memory budget and container ceilings

Compose memory limits are ceilings for individual containers. They do not reserve that amount from physical RAM when a container starts, and several containers can use less than their limits when idle. A container can still be OOM-killed if its working set reaches its ceiling. Treat totals as maximum combined use for the listed containers, not a reservation or a whole-host capacity estimate.

The `glance-live` dashboard gateway uses one Rust/Tokio event-loop thread and at most two blocking DNS workers, with
configured ceilings of 96 MiB memory, 0.50 CPU, and 32 processes. Its
`GLANCE_LIVE_MEMORY_LIMIT` and `GLANCE_LIVE_CPUS` settings are adjustable in
`.env`; the Compose process limit is fixed at 32. The memory ceiling remains
96 MiB to retain headroom for varied workloads, so the Rust migration does not
change the configured Compose memory total. The gateway does not use a GPU.
Its defaults also bound active SSE streams to 64, accepted HTTP connections to
128, provider bodies to 2 MiB, shared snapshots to 256 KiB, torrent entries to
2,000, Jellyfin sessions to 256, and SSE connections to 900 seconds. These
application bounds reduce unbounded growth but do not guarantee a particular
working set or host capacity; oversized or failed upstream updates mark the
topic stale and preserve its last valid snapshot.

The checked-in default `mem_limit` values across the Compose files sum to **16,256 MiB (15.875 GiB)**. The nine optional Nextcloud AIO child limits below add **7,680 MiB (7.5 GiB)**, for a combined ceiling of **23,936 MiB (23.375 GiB)**. This is a configured upper bound, not expected usage or a safe host-RAM recommendation. The Compose total changes if you edit the limits.

The Rust `jellyfin-counts-cache` has configured ceilings of 64 MiB, 0.25 CPU,
and 32 processes. It uses no GPU, runs as UID/GID `1000:1000`, and has a
read-only root filesystem. Its current-thread Tokio runtime allows at most two
blocking DNS workers and eight concurrent HTTP client tasks. Each request is
limited to 8 KiB and 32 headers with a five-second lifetime; upstream JSON is
limited to 16 KiB and a 30-second timeout. It keeps only the six serialized
count fields in memory. These are configuration and implementation bounds, not
measured working-set or CPU figures. Measure this container on the target host
with `docker stats --no-stream jellyfin-counts-cache` before drawing conclusions
about resource savings.

## Glance live migration measurements

The October 16, 2026 migration check compared the original Python/aiohttp
gateway with the Rust release image in Docker Desktop Linux containers. Both
ran without external network access, as UID/GID 10001, with a read-only root,
all capabilities dropped, and identical 96 MiB, 0.50 CPU, and 32-process limits.
The load fixture used four Downloads and four Media SSE clients, 1,000 fake
torrents, one Jellyfin session, two-second polling, and WebSockets disabled.
The fixture process ran in a separate container sharing only the gateway's
isolated network namespace. It was excluded from the gateway measurements.

| Sample | Original Python gateway | Rust gateway |
| --- | ---: | ---: |
| Idle process RSS (`/proc/1/status`) | 44.1 MiB | 3.8 MiB |
| Active process RSS | 43.9 MiB | 7.7 MiB |
| Active container memory (`docker stats`) | 31.3 MiB | 4.5 MiB |

These were short, sampled measurements, not maximum-memory or long-duration
benchmarks. The active container sample was about 85% smaller with Rust.
Docker's displayed memory excludes reclaimable cache; RSS includes resident
file-backed pages, so the two measures are not interchangeable. Both probes
delivered 40 successful snapshots across eight clients and confirmed that
collectors stopped after disconnects. This did not exercise the real Glance,
Jellyfin, qBittorrent, TLS, or sustained WebSocket deployment workload. No CPU
speedup is claimed from these samples; the gateway has no GPU access.

The reusable [Rust load probe](../glance/live/src/bin/glance-live-probe.rs)
uses fake loopback services and the gateway's existing locked dependencies.
The [Rust CI job](../.github/workflows/security-audit.yml) shows the disposable
runtime/fixture commands. Build the separate Docker `probe` target or use
`cargo run --locked --features fixture-probe --bin glance-live-probe -- --seconds 10`
from `glance/live` beside an isolated gateway configured as shown in CI.
The probe is excluded from the default production image. The gateway and its
test tooling require no Python interpreter.

## Jellyfin counts cache validation

The October 16, 2026 Rust release-container check used a fake loopback Jellyfin
service in an isolated network namespace, with no external network, host media,
or Docker socket. The cache ran as UID/GID 1000 with a read-only filesystem,
all capabilities dropped, and its 64 MiB, 0.25 CPU, and 32-process ceilings.
It served 800 successful cached responses across eight concurrent clients.
After that short run, `/proc/1/status` reported **3.4 MiB RSS and one thread**.
The separate Rust fixture was excluded from the sample. The runtime contained
neither Python nor Cargo and had no GPU device requests. This sample does not
measure peak memory, sustained CPU use, real Jellyfin, DNS, or HTTPS workloads;
there is no Python baseline or claimed percentage saving for this cache.

## Subtitle Studio Rust validation

The October 16, 2026 CPU release-container check used the public whisper.cpp
JFK audio fixture, converted to a disposable MKV, with `tiny-q5_1`, two CPU
threads, one pipeline worker, a 1 GiB ceiling, and a 2 CPU quota. It ran as
UID/GID 1000 with a read-only root, all capabilities dropped, no external
network access, and no host media or Docker socket. The Rust probe confirmed
two timed source cues, a valid SRT download, and transcript cache reuse on a
second job. The runtime contained no Python interpreter.

Idle server RSS was **3.3 MiB**, and server RSS after transcription was
**3.6 MiB**. The highest cgroup memory sample observed while polling every
250 ms was **127.4 MiB**, including the separate probe process and reclaimable
file cache. Transcription and the cached follow-up completed in about 2.3
seconds; cumulative container CPU time was 2.9 seconds. No OOM kill occurred.
This short quantized-tiny fixture is not a peak-memory benchmark for movies,
the default `small-q5_1` model, LM Studio, CUDA or translation quality. There
is no comparable Python baseline or claimed percentage reduction.

## Subtitle Studio full large-v3 CUDA validation

A separate short CUDA validation used the full official `large-v3` GGML model,
a public JFK fixture converted to MKV, and the isolated CUDA test image on an
RTX 3060 with 12 GiB VRAM. It ran as UID/GID 1000 with a read-only root, all
capabilities dropped, no network, a 6 GiB memory ceiling, a 2 CPU quota,
and a 64-process ceiling. The fixture input and preloaded model were read-only mounts;
job data and outputs used temporary filesystems. A fresh transcription
produced three source cues and a valid SRT. A second job reused the transcript
cache. The production CUDA image was separately checked to contain no Python,
Cargo, Rust compiler, probe, or unit-test executable.

| Measure | Observed value |
|---|---:|
| Elapsed time for both jobs | 11.34 seconds |
| Container CPU time | 3.52 seconds |
| Server RSS before / after transcription | 3.44 / 3.59 MiB |
| Sampled container memory | 351.5 MiB |
| Kernel-recorded container memory peak | 369.2 MiB |
| Whole-device VRAM at start / observed peak | 1007 / 5102 MiB |
| OOM events | 0 |

The VRAM numbers are whole-device `nvidia-smi` readings and include other GPU
applications; they do not isolate Subtitle Studio's allocation. The short
fixture does not establish movie-length peak memory, transcription quality,
named-volume cold-start behavior, a minimum safe limit, or a Python comparison.
Filesystem cache accounting and cold versus warm model state affect container
memory samples. Keep 6 GiB as a starting ceiling until representative media
and concurrent LM Studio use have been measured on the target host. The
[component guide](../subtitle-studio/README.md) documents the model and CUDA
setup.

The optional Docker `test` target includes the
[Rust fixture probe](../subtitle-studio/src/bin/subtitle-studio-probe.rs).
Production excludes the probe. The server unloads Whisper between bounded
audio chunks and before LLM translation/review. The UI's Resources disclosure
and `/api/resources` provide on-demand samples; unavailable measurements stay
null, and CUDA GPU samples cover the whole device, including other apps.

## Example application ceilings

The root `.env.example` provides these adjustable ceilings for selected workloads:

| Container | Default ceiling |
|---|---:|
| Open WebUI | 1280 MiB |
| Subtitle Studio | 1 GiB CPU default; 6 GiB suggested starting ceiling for full `large-v3` CUDA |
| Stirling PDF | 1536 MiB |
| TubeArchivist Elasticsearch | 1536 MiB |
| Subforge default, OCR disabled | 1536 MiB |
| Subforge text-only option | 512 MiB |

The first four values can be changed through `OPEN_WEBUI_MEMORY_LIMIT`, `SUBTITLE_STUDIO_MEMORY_LIMIT`, `STIRLING_PDF_MEMORY_LIMIT`, and `ARCHIVIST_ES_MEMORY_LIMIT`. Subtitle Studio defaults to `1g`, `2` CPUs, and 64 processes. If the canonical Studio ceiling settings are unset, Compose accepts the legacy `SUBTITLE_WORKER_MEMORY_LIMIT` and `SUBTITLE_WORKER_CPUS` variables; canonical values take precedence when both forms are set. Whisper is CPU-only by default with two threads, one pipeline worker, and 300-second WAV chunks; the worker releases its model between chunks before translation/review. CUDA is available only through the optional CUDA Compose override. Full `large-v3` uses a suggested `6g` worker ceiling as a starting point; one short CUDA fixture stayed below this limit, but that run does not establish a minimum safe ceiling. GPU VRAM is a separate shared resource. These configured bounds do not predict a specific working set; translation model memory belongs to the LM Studio process. Subforge uses `CONVERTER_MEMORY_LIMIT`; its Compose and `.env.example` default is `1536m` with image OCR disabled. Text-only deployments can lower it to `512m` after considering their formats and workload. Image-subtitle OCR is opt-in with `CONVERTER_ENABLE_OCR=true` and needs the `1536m` ceiling. Keep OCR disabled to lock image-based subtitle extraction; some text sidecars, including TTML and SBV, still use the bounded Subtitle Edit converter when OCR is off. Recreate the affected container after changing a Compose environment value or memory limit. OCR and other converter child processes count against the same container ceiling.

Subforge converts one input at a time and limits the OCR OpenMP thread count. Its four HTTP workers and job queue are bounded, but that does not reserve memory for the host or reduce memory used by unrelated services.

## Optional Nextcloud AIO child limits

The helper targets a fixed allowlist of nine Nextcloud AIO children. Their configured ceilings total 7680 MiB (7.5 GiB):

| AIO child | Ceiling |
|---|---:|
| `nextcloud-aio-apache` | 768 MiB |
| `nextcloud-aio-database` | 1024 MiB |
| `nextcloud-aio-eurooffice` | 3072 MiB |
| `nextcloud-aio-imaginary` | 256 MiB |
| `nextcloud-aio-nextcloud` | 1536 MiB |
| `nextcloud-aio-notify-push` | 128 MiB |
| `nextcloud-aio-redis` | 128 MiB |
| `nextcloud-aio-talk` | 384 MiB |
| `nextcloud-aio-whiteboard` | 384 MiB |

That 7.5 GiB sum is only the upper bound for these nine children. It excludes the AIO master container, other containers, Docker Desktop's Linux VM on Windows/macOS, the host operating system, and host applications such as LM Studio. Measure actual peak use on your machine and leave additional headroom before applying ceilings. Container limits alone cannot determine a safe LM Studio allocation; monitor its process alongside Docker workloads.

The AIO helper is read-only by default: it reports current limits and the fixed targets for containers it finds. `--apply` saves the previous memory and swap limits to a restore point in the system temporary directory, applies targets to matching existing AIO children, and verifies them. `--restore` restores those saved values. It refuses to restore when a target container was recreated, preventing an old restore point from being applied to a new container. The script does not change AIO configuration, so AIO child limits do not persist when AIO recreates those children.

## Apply the Compose project ceilings

`scripts/compose_memory_limits.py` reports the checked-in Compose defaults and current limits for containers labelled as part of the `homelab` Compose project. It is a separate helper from the AIO child helper above. Its default report is read-only. `--apply` changes limits only on matching containers that currently exist, saves their previous memory and swap values in a restore point under the system temporary directory, then verifies the changes. It reads the Compose default expressions in the repository; it does not load the ignored `.env`, so review the report and confirm the checked-in defaults are what you intend before applying. These runtime Docker updates do not edit Compose files and can be replaced when containers are recreated. `--restore` restores the saved values and refuses stale restore points if a container was recreated or relabeled.

Run the commands from the repository root after Docker is available. Start with dry runs for both groups:

```powershell
py -3 .\scripts\compose_memory_limits.py
py -3 .\scripts\nextcloud_aio_memory_limits.py
```

If the reported ceilings fit the workload you measured, apply each set:

```powershell
py -3 .\scripts\compose_memory_limits.py --apply
py -3 .\scripts\nextcloud_aio_memory_limits.py --apply
```

Restore each helper's saved values with:

```powershell
py -3 .\scripts\compose_memory_limits.py --restore
py -3 .\scripts\nextcloud_aio_memory_limits.py --restore
```

On macOS/Linux, use `python3` with the same script arguments:

```shell
python3 scripts/compose_memory_limits.py
python3 scripts/nextcloud_aio_memory_limits.py
python3 scripts/compose_memory_limits.py --apply
python3 scripts/nextcloud_aio_memory_limits.py --apply
python3 scripts/compose_memory_limits.py --restore
python3 scripts/nextcloud_aio_memory_limits.py --restore
```

The Windows PowerShell wrapper `scripts/nextcloud-aio-memory-limits.ps1` remains available for AIO-only operations. It accepts no arguments for a dry run, `-Apply` to apply the caps, and `-Restore` to restore saved limits. The Python examples above work on both platforms when Python 3 and Docker CLI are installed.

Review the default reports first. Apply changes only when the reported targets fit your measured workload and available host memory. If AIO recreates children after applying limits, inspect their new limits again; the old restore point is intentionally tied to the original container identities.

## Observe actual use

Use `docker stats --no-stream` to compare current container usage with each ceiling. Windows and macOS users should also check Docker Desktop's resource settings and host process/memory monitor because the containers share its Linux VM. On Linux, check Docker container use alongside host memory and swap. LM Studio runs on the host in this example, so observe it with the host operating system's process monitor rather than expecting it in Docker statistics.

Example ceilings are starting points, not measured recommendations. Increase a limit only when logs or Docker's OOM state show that a workload needs more memory, and check total host pressure after the change.
