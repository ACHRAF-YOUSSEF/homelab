# Security

## Reporting a vulnerability

Please do not post credentials, tokens, private deployment details, or an unpatched vulnerability in a public issue. If GitHub private vulnerability reporting is enabled for this repository, use its private reporting form. Otherwise, use a private contact route already available to you; this repository does not promise a dedicated security response channel or response time.

If a secret was committed, revoke or rotate it first. Removing it from the current files does not remove it from Git history. Coordinate any history rewrite with repository collaborators before changing shared history.

## Automated audits

The [security audit workflow](.github/workflows/security-audit.yml) runs on pushes, pull requests, manual dispatch, and its scheduled weekly run. The workflow has no write permissions or deployment credentials. Its audit jobs are all gating: failures stop the check and findings are not blanket-suppressed. The workflow does not require license credentials. Review the workflow for exact tool versions, flags, and file coverage.

Gitleaks findings are fully redacted. The privacy checker prints file, line, and rule identifiers without matched values. Bandit prints finding locations, rule identifiers, severity, and confidence without source excerpts.

Some checks can fail on the first run because they report existing findings. The October 9, 2026 local snapshot included two high-severity pnpm advisories: `source-map-js` 1.2.1 (CVE-2026-93749, fixed in 1.2.2) and `braces` 3.0.3 (CVE-2026-93687, no fix reported); eight medium Bandit findings in the subtitle worker and Glance counts helper; and two high Trivy Dockerfile findings (`DS-0002`, missing a non-root `USER`) in `docker-proxy/aio/Dockerfile` and `subtitle-worker/Dockerfile`. These findings are not suppressed or accepted as exceptions. The snapshot may become stale as dependencies and code change.

## Deployment notes

Review the root [homelab guide](README.md#docker-api-access) and component guides before exposing services. The subtitle worker currently has no authentication and listens on all interfaces; its Compose port is published on all host interfaces. Restrict access with host firewall rules or a loopback-only local override when remote clients do not need it. A URL supplied to a service can cause it to make outbound requests; do not expose untrusted URL inputs to a service with access to sensitive networks or files. Docker socket proxies reduce API access but still grant the endpoint permissions configured for each proxy. Check each service's mounts, networks, and proxy rules before changing exposure. Trivy scans dependency manifests for vulnerabilities and Dockerfiles for misconfiguration; this workflow does not scan Compose configuration for misconfiguration or inspect operating-system vulnerabilities in the base images supplied to containers.
