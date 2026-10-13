# Memory budget and container ceilings

Compose memory limits are ceilings for individual containers. They do not reserve that amount from physical RAM when a container starts, and several containers can use less than their limits when idle. A container can still be OOM-killed if its working set reaches its ceiling. Treat totals as maximum combined use for the listed containers, not a reservation or a whole-host capacity estimate.

The checked-in default `mem_limit` values across the Compose files sum to **18,304 MiB (17.875 GiB)**. The nine optional Nextcloud AIO child limits below add **7,680 MiB (7.5 GiB)**, for a combined ceiling of **25,984 MiB (25.375 GiB)**. This is a configured upper bound, not expected usage or a safe host-RAM recommendation. The Compose total changes if you edit the limits.

## Example application ceilings

The root `.env.example` provides these adjustable ceilings for selected workloads:

| Container | Default ceiling |
|---|---:|
| Open WebUI | 1280 MiB |
| Subtitle Studio worker | 3 GiB |
| Stirling PDF | 1536 MiB |
| TubeArchivist Elasticsearch | 1536 MiB |
| Subforge default, OCR disabled | 1536 MiB |
| Subforge text-only option | 512 MiB |

The first four values can be changed through `OPEN_WEBUI_MEMORY_LIMIT`, `SUBTITLE_WORKER_MEMORY_LIMIT`, `STIRLING_PDF_MEMORY_LIMIT`, and `ARCHIVIST_ES_MEMORY_LIMIT`. Subforge uses `CONVERTER_MEMORY_LIMIT`; its Compose and `.env.example` default is `1536m` with image OCR disabled. Text-only deployments can lower it to `512m` after considering their formats and workload. Image-subtitle OCR is opt-in with `CONVERTER_ENABLE_OCR=true` and needs the `1536m` ceiling. Keep OCR disabled to lock image-based subtitle extraction; some text sidecars, including TTML and SBV, still use the bounded Subtitle Edit converter when OCR is off. Recreate the affected container after changing a Compose environment value or memory limit. OCR and other converter child processes count against the same container ceiling.

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
