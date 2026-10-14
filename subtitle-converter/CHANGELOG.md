# Subforge revisions

## 0.4.0

- Refresh the dashboard with an ember-accented dark palette, compact pipeline summary, and clickable stage filters.
- Keep inventory search, filter, and pagination state per browser tab, and add a per-track drawer for embedded and sidecar subtitle selection.
- Report tracked SRT output counts separately from tracked processed-input totals; show totals as unavailable when bounded tracking cannot provide an exact answer.
- Add bounded retries for recorded failed paths, observed-request integration indicators, Jellyfin library names with IDs, and live resource bars.
- Scope selected-track failure records to the current source fingerprint and clear only failures resolved by a successful whole-file pass.
- Publish fixed local CPU and CUDA image tags for `0.4.0`; Compose uses the prebuilt selected edition.

## 0.3.1

- Split Subforge into fixed local tags `homelab-subtitle-converter:0.3.1-cpu` and `homelab-subtitle-converter:0.3.1-cuda`; Compose starts prebuilt images and never builds or pulls them.
- Start the service with the regular Compose stack after building the selected local image; use the CUDA override when running the CUDA edition.
- Compile the image edition into the Rust binary and report it in the API and dashboard; GPU OCR requires the CUDA edition plus explicit runtime permissions and an allocated GPU worker.
- Keep the CPU image Python-free, build the CUDA Rust binary in its own stage, and isolate locked Python/PaddleOCR tools in a separately cacheable stage.

## 0.3.0

- Add persisted CPU/OCR allocations, pause/resume, and live file-worker monitoring in Settings → Workers.
- Generate a private workers.conf with default counts and ceilings when missing; UI saves update that file instead of worker environment variables.
- Run independent files in a bounded parallel pool while serializing state, integration notifications, and conflicting subtitle destinations.
- Limit concurrency by deployment ceilings, the actual CPU allowance, and a conservative memory admission budget.
- Keep per-tool CPU threads limited to one and preserve cancellation, source files, and existing SRT outputs.
- Add optional NVIDIA GPU OCR using a separate linux/amd64 image, one explicitly granted device, and a default-zero GPU worker allocation.
- Lock 81 Python dependency pins across two files, verify all 15 bundled model archives against their SHA-256 manifest, and allow optional hash-verified wheel and model-archive BuildKit caches without adding cache payload to the runtime image.
- Validate GPU OCR with a 200-distinct-cue synthetic SUP fixture on one RTX 3060: one GPU worker observed, source preserved, cached repeat confirmed, 9.32 seconds including startup and repeat, and 1,097,826,304 bytes (about 1,047 MiB) peak container memory under a 1536 MiB ceiling. This does not establish full-library peaks or results on other GPUs/drivers.

## 0.2.4

- Update uptime every second and poll live resource measurements while Status is visible.
- Report measured container CPU allowance, CPU/RAM usage, process RSS, active jobs, and metadata-cache usage separately from execution limits.
- Derive cache maxima from the cache implementation and keep status responses available during conversion.
- Display unavailable measurements honestly on unsupported or shared-host setups.

## 0.2.3

- Preserve completed text passes in the dashboard total when later OCR fails, including passes that only queued image tracks.
- Label the dashboard and Glance totals as processed files, because they count completed input passes rather than generated SRT files.

## 0.2.2

- Keep previously converted files in the dashboard count after a later OCR failure when their earlier SRT outputs are retained in state.
- Wrap long task checkbox labels on narrow screens.

## 0.2.1

- Combine Tasks, Status, languages, media management, and integrations under Settings.
- Redirect existing System bookmarks to their Settings sections.
- Fix the Docker image tag and label to the code revision; remove the configurable image tag.
- Show scheduled tasks as cards on mobile.

## 0.2.0

- Add persisted schedules, task controls, local health checks, cache cleanup, and bounded private backups.
- Separate text discovery from daily OCR for new or changed files.
- Cache track metadata and retain library inventory across restarts.
- Keep one conversion worker and one OCR thread, with a default container ceiling of one CPU.
