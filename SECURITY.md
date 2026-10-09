# Security

## Reporting a vulnerability

Please do not post credentials, tokens, private deployment details, or an unpatched vulnerability in a public issue. If GitHub private vulnerability reporting is enabled for this repository, use its private reporting form. Otherwise, use a private contact route already available to you; this repository does not promise a dedicated security response channel or response time.

If a secret was committed, revoke or rotate it first. Removing it from the current files does not remove it from Git history. Coordinate any history rewrite with repository collaborators before changing shared history.

## Automated audits

The [security audit workflow](.github/workflows/security-audit.yml) runs on pushes, pull requests, manual dispatch, and its scheduled weekly run. The workflow has no write permissions or deployment credentials. Its audit jobs are all gating: failures stop the check and findings are not blanket-suppressed. The workflow does not require license credentials. Review the workflow for exact tool versions, flags, and file coverage.

Gitleaks findings are fully redacted. The privacy checker prints file, line, and rule identifiers without matched values. Bandit prints finding locations, rule identifiers, severity, and confidence without source excerpts.

The October 9, 2026 initial audit snapshot found two high-severity pnpm advisories: `source-map-js` 1.2.1 (CVE-2026-93749, fixed in 1.2.2) and `braces` 3.0.3 (CVE-2026-93687); eight medium Bandit findings in the subtitle worker and Glance counts helper; and two high Trivy `DS-0002` Dockerfile findings for missing non-root users. None were suppressed or accepted as exceptions. Remediation pinned `source-map-js` to 1.2.2, removed the vulnerable Tailwind CLI dependency chain, routed configured HTTP requests through a shared boundary client, and added non-root users to both Dockerfiles.

The final pinned audit on October 9 passed: Gitleaks 8.30.1 found zero leaks in current public files and all 68 reachable commits; all 14 privacy tests and the repository privacy checker passed; actionlint 1.7.12 passed; Bandit 1.9.4 at medium severity and confidence reported zero findings; pip-audit 2.10.1 strict found zero known Python dependency vulnerabilities; pnpm 12.10.1 audit found zero moderate-or-higher advisories; and Trivy 0.74.0 found zero high or critical dependency-manifest or Dockerfile findings. No findings were allowlisted or suppressed.

Validation also passed 84 Python tests, including five HTTP-boundary regression tests, and 35 Node tests. CSS build/watch parity and dynamic addition and removal of watched sources passed. The isolated worker and proxy runtime checks passed. The full AIO install, backup, restore, update lifecycle, and GPU transcription flow were not tested.

## Deployment notes

Review the root [homelab guide](README.md#docker-api-access) and component guides before exposing services. The subtitle worker has no authentication. Native mode binds to loopback by default; Compose publishes the host port on loopback by default while the container listener remains available to its Docker network. Set `SUBTITLE_BIND_ADDRESS` deliberately before exposing it to other hosts. The Jellyfin counts helper runs as UID/GID `1000:1000`, binds to the homelab network in Compose, and has no published host port. The shared HTTP client accepts valid HTTP and HTTPS service URLs, verifies TLS normally, rejects embedded credentials and malformed URLs, and does not follow redirects. The AIO proxy runs as UID/GID `99:33`, drops all Linux capabilities, and uses `network_mode: none`; its Unix socket grants the lifecycle permissions configured for AIO. Check each service's mounts, networks, and proxy rules before changing exposure. Trivy scans dependency manifests for vulnerabilities and Dockerfiles for misconfiguration; this workflow does not scan Compose configuration for misconfiguration or inspect operating-system vulnerabilities in supplied images.
