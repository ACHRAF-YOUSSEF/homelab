# Security

## Reporting a vulnerability

Please do not post credentials, tokens, private deployment details, or an unpatched vulnerability in a public issue. If GitHub private vulnerability reporting is enabled for this repository, use its private reporting form. Otherwise, use a private contact route already available to you; this repository does not promise a dedicated security response channel or response time.

If a secret was committed, revoke or rotate it first. Removing it from the current files does not remove it from Git history. Coordinate any history rewrite with repository collaborators before changing shared history.

## Automated audits

The [security audit workflow](.github/workflows/security-audit.yml) runs on pushes, pull requests, manual dispatch, and its scheduled weekly run. The workflow has no write permissions or deployment credentials. Its audit jobs are all gating: failures stop the check and findings are not blanket-suppressed. The workflow does not require license credentials. Review the workflow for exact tool versions, flags, and file coverage.

Gitleaks findings are fully redacted. The privacy checker prints file, line, and rule identifiers without matched values. Bandit prints finding locations, rule identifiers, severity, and confidence without source excerpts. The Rust Glance gateway uses a separate locked RustSec audit; its dependency audit and build checks do not replace the repository privacy or secret scans.

The October 9, 2026 initial audit snapshot found two high-severity pnpm advisories: `source-map-js` 1.2.1 (CVE-2026-93749, fixed in 1.2.2) and `braces` 3.0.3 (CVE-2026-93687); eight medium Bandit findings in Subtitle Studio and the then-Python Glance counts helper; and two high Trivy `DS-0002` Dockerfile findings for missing non-root users. None were suppressed or accepted as exceptions. Remediation pinned `source-map-js` to 1.2.2, removed the vulnerable Tailwind CLI dependency chain, routed configured HTTP requests through a shared boundary client, and added non-root users to both Dockerfiles. The counts helper has since migrated to Rust and is covered by its own locked Rust checks and RustSec audit.

The final pinned audit snapshot on October 9 passed: Gitleaks 8.30.1 found zero leaks in current public files and all 68 reachable commits; all 14 privacy tests and the repository privacy checker passed; actionlint 1.7.12 passed; Bandit 1.9.4 at medium severity and confidence reported zero findings; pip-audit 2.10.1 strict found zero known Python dependency vulnerabilities; pnpm 12.10.1 audit found zero moderate-or-higher advisories; and Trivy 0.74.0 found zero high or critical dependency-manifest or Dockerfile findings. No findings were allowlisted or suppressed. This dated snapshot predates the Rust gateway migration; the workflow is the source for validation of the current revision.

That snapshot also passed 84 Python tests, including five HTTP-boundary regression tests, and 35 Node tests. CSS build/watch parity and dynamic addition and removal of watched sources passed. The isolated worker and proxy runtime checks passed. The full AIO install, backup, restore, update lifecycle, and GPU transcription flow were not tested. The migrated Glance gateway is checked separately with Rust formatting, Clippy, tests, a locked RustSec audit, and an isolated unprivileged runtime/proxy fixture under its configured resource ceilings; consult the workflow for the current run and result.

## Deployment notes

Local validation of the October 16, 2026 Glance gateway migration passed 8 Rust
unit tests and 9 HTTP/WebSocket fixture tests on Windows and Linux, formatting,
Clippy with warnings denied, and the locked RustSec audit. A scoped Trivy scan
found no high/critical Cargo vulnerabilities or Dockerfile misconfigurations;
a fully redacted, network-disabled Gitleaks scan of migration sources, fixtures,
the load probe, and `.env.example` found no leaks. The 14 privacy tests,
repository privacy checker, 11 memory-helper tests, dashboard JavaScript syntax,
actionlint, and Compose validation using a disposable `.env.example` copy passed.
An isolated runtime probe passed under 96 MiB/0.50 CPU/32-process ceilings,
including collector cleanup and graceful SIGTERM shutdown. These local checks
do not constitute a full-history secret scan, an image OS vulnerability scan,
or a test of the deployed stack.

The gateway load probe is also Rust. Its `fixture-probe` feature and separate
Docker `probe` target keep test tooling out of the default production image.
The Rust-only probe passed the same eight-client/1,000-torrent isolated check;
both runtime and probe images were checked for absence of Python executables.

Review the root [homelab guide](README.md#docker-api-access) and component guides before exposing services. The Glance dashboard and gateway have no user authentication and should remain LAN-only. `glance-live` runs as UID/GID `10001:10001`, drops all Linux capabilities, has a read-only filesystem, and has no GPU device access. It joins only `homelab`, does not mount the Docker socket, and is limited by default to 96 MiB, 0.50 CPU, and 32 processes. It bounds active clients and upstream response/snapshot sizes; on provider errors or cap violations it reports stale state and keeps the last valid snapshot. The health route checks gateway availability, not provider health. The Rust Jellyfin counts cache runs as UID/GID `1000:1000`, has a read-only root filesystem, drops all Linux capabilities, joins `homelab`, publishes no host port, and has 64 MiB, 0.25 CPU, and 32-process ceilings with no GPU access. Its health check remains unhealthy until the first valid count response; afterward it reflects cached data readiness while refresh failures retain the last good counts. It accepts HTTP or HTTPS Jellyfin base URLs, verifies TLS normally, rejects embedded URL credentials, and does not follow redirects or use configured HTTP proxies. Subtitle Studio's shared HTTP client accepts valid HTTP and HTTPS service URLs, verifies TLS normally, rejects embedded credentials and malformed URLs, and does not follow redirects. The AIO proxy runs as UID/GID `99:33`, drops all Linux capabilities, and uses `network_mode: none`; its Unix socket grants the lifecycle permissions configured for AIO. Check each service's mounts, networks, and proxy rules before changing exposure. Trivy scans dependency manifests for vulnerabilities and Dockerfiles for misconfiguration; this workflow does not scan Compose configuration for misconfiguration or inspect operating-system vulnerabilities in supplied images.
