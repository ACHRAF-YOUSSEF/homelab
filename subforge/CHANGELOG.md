# Subforge revisions

## Unreleased

- Enable scheduled media inventory refresh by default and migrate legacy task state once, while preserving later manual opt-outs.
- Show Bazarr inventory age and restart-based staleness guidance, with an on-demand sync action.
- Keep Library Fit at a 25-row minimum with a scrolling pane; add page selection and reviewed bulk conversion capped at 64 explicit media paths.
- Unify settings, task-schedule, and worker-allocation saves with a shared sticky save/discard bar and unsaved-navigation warning.
- Clarify global failed-file totals, task-result scope, and notification send timestamps, including limits of legacy records.
- Show live connection state and update age from the latest valid status, system, or worker message received by the browser, with an absolute timestamp on hover. Live badges share that clock; stale resource status uses its own last-sample time.
- Improve keyboard focus, compact text size, and Library filename wrapping; use explicit Hide/Show wording for the sidebar toggle, and show one edition badge in the header with plain-language System guidance. Explain SDH in desktop copy and the drawer, and spell it out in mobile coverage chips.
- Group scan roots by available storage-volume identity. Generic groups use numbered **Storage volume** names and plain folder names; Windows drive labels remain recognizable. Optional `CONVERTER_*_SOURCE` hints only supply a validated Windows drive label; they do not grant host-path access or expose the configured host paths. Clarify that container limit changes require recreation, not restart.

## 0.4.4 — 2026-10-16

- Update locked rustls to 0.23.46, fixing RustSec advisories RUSTSEC-2026-0346 through RUSTSEC-2026-0349 without audit suppressions.
- Replace Python/Paddle OCR with a native Rust `ocr-rs` 2.4.1 helper using checksum-pinned MNN 3.4.1 source and 25 immutable offline model files; CPU and CUDA runtime/test images no longer include Python.
- Pin the CUDA build's CUTLASS source archive by commit and SHA-256, with an optional `cuda-source-cache` build context and a CMake source override that avoids cloning Git history.
- Correct Arabic OCR's visual-to-logical character order while preserving left-to-right runs and combining marks; isolate backend-health child output so it cannot contaminate JSON.
- Verify direct CPU helper recognition against exact English, French, and Arabic fixture phrases and pass all 11 helper tests with strict Clippy; the full CPU SUP-to-SRT pipeline now passes for all three languages and preserves existing SRT files.
- Pass 512 MiB text/resource and 1.5 GiB OCR resource fixtures. Direct CUDA recognition in the deployed image matches exact English, French, and Arabic fixture text. The full CUDA SUP-to-SRT smoke passes for 200 English cues plus French and Arabic fixtures, with the CUDA backend and GPU worker observed, sources preserved, and cached repeat confirmed. GPU sessions reject sessions that MNN silently creates on CPU after CUDA setup fails, while individual unsupported operators may still execute on CPU within an admitted CUDA session. Runtime performance benchmarking remains pending.
- Bound concurrent selected-file jobs by global worker admission while keeping full-library scans and inventory/maintenance work exclusive; expose active jobs while retaining `running` compatibility.
- Persist and deduplicate validated Bazarr SRT publication receipts, scan after the queue drains, and verify uncertain attempts without blind replay; classify raw socket timeouts consistently and treat transport loss after a scan PATCH as uncertain while keeping explicit HTTP and connection-refused errors definite.
- Publish local `homelab-subforge:0.4.4-cpu` and `homelab-subforge:0.4.4-cuda` image tags.
## 0.4.3 — 2026-10-15

- Prioritize conversion and local worker controls together on Overview, with a compact library summary and actionable failure and OCR counts.
- Remove duplicate recent activity, OCR totals, local-node information, and empty idle worker panels from the Overview layout.
- Keep active jobs and file workers visible while running; group remote nodes, setup, and operation logs in an expandable diagnostics section.
- Improve responsive spacing, worker steppers, resource meters, pause controls, and readable admission reasons.
- Publish matching local `0.4.3-cpu` and `0.4.3-cuda` image tags.

## 0.4.2 — 2026-10-15

- Fit Library and Activity tables to the available viewport, with internal row scrolling, fixed pagination, and an adaptive Fit page size.
- Replace Activity's long job feed with a paginated jobs table and a details drawer for full errors, results, retries, and worker history.
- Move local worker allocation, active workers, and remote nodes onto Overview; redirect existing Workers settings bookmarks.
- Preserve table filters and page choices per browser tab and keep mobile controls accessible.
- Publish matching local `0.4.2-cpu` and `0.4.2-cuda` image tags.

## 0.4.1 — 2026-10-15

- Include the assigned node, CPU/CPU OCR/GPU OCR resource, and worker ID in extraction phase, completion, failure, and remote retry logs; retain executor details in job history.
- Confirm published SRTs against Bazarr inventory after uncertain scan responses, allow longer scoped scans, and show pending indexing without repeating extraction.
- Identify Subforge by its current name and blue icon in Glance's Overview Docker list.
- Publish matching local `0.4.1-cpu` and `0.4.1-cuda` image tags.

## 0.4.0

- Refresh the dashboard with an ember-accented dark palette, compact pipeline summary, and clickable stage filters.
- Keep inventory search, filter, and pagination state per browser tab, and add a per-track drawer for embedded and sidecar subtitle selection.
- Report tracked SRT output counts separately from tracked processed-input totals; show totals as unavailable when bounded tracking cannot provide an exact answer.
- Add bounded retries for recorded failed paths, observed-request integration indicators, Jellyfin library names with IDs, and live resource bars.
- Scope selected-track failure records to the current source fingerprint and clear only failures resolved by a successful whole-file pass.
- Publish fixed local CPU and CUDA image tags for `0.4.0`; Compose uses the prebuilt selected edition.

## 0.3.1

- At this historical release, before the service rename, split Subforge into former fixed local tags `homelab-subtitle-converter:0.3.1-cpu` and `homelab-subtitle-converter:0.3.1-cuda`; Compose started those prebuilt images and never built or pulled them.
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
- Historical PaddleOCR validation for 0.3.0 only: a 200-distinct-cue synthetic SUP fixture on one RTX 3060 observed one GPU worker, preserved the source, confirmed a cached repeat, took 9.32 seconds including startup and repeat, and used 1,097,826,304 bytes (about 1,047 MiB) peak container memory under a 1536 MiB ceiling. These Python/PaddleOCR measurements do not describe the native `ocr-rs`/MNN implementation introduced in 0.4.4 and do not establish full-library peaks or results on other GPUs/drivers.

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
