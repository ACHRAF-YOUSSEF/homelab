# Subforge revisions

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
