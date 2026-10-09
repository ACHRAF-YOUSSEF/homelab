# Repository guidance

This is a reusable homelab example. Keep implementation and documentation in sync.

## Reusable agents

Use the personal agents installed in the user-level Codex `agents/` directory
when available. See [.codex/agents/README.md](.codex/agents/README.md) for global
setup and usage. Reuse an existing specialist session with follow-up tasks before
spawning another agent for the same role. New chats reuse the saved profiles;
they do not retain a previous agent's conversation. If a personal profile is
unavailable, assign the same scoped responsibility to an available agent.

| Role | Model | Work |
| --- | --- | --- |
| `repo_explorer` | `gpt-6-luna`, medium | Routine searches, service maps, dependency inventory |
| `docs_maintainer` | `gpt-6-luna`, medium | Scoped Markdown edits, platform examples, link checks |
| `validation_runner` | `gpt-6-luna`, medium | Scoped tests, workflow linting, privacy and config checks |
| `security_reviewer` | `gpt-6.1-sol`, high | Docker access, CI trust boundaries, leak/security review |

The project default for subagents is the smaller model; the role definitions
are personal global configuration rather than repository files. Assign a
stronger model for ambiguous or security-sensitive reasoning. Give each agent
a clear file scope; agents share the checkout, so avoid concurrent edits to
the same files.

## Privacy and Docker access

- Keep deployment addresses, private links, and real credentials in the ignored
  root `.env`; never read or print that file while doing example-repository work.
- Keep credential entries blank in `.env.example`; use generic local defaults
  for deployment endpoints and public upstream URLs for example feed sources.
- Glance YAML must use environment references for endpoints, domains, IPs,
  external links, search providers, and RSS sources.
- Only Docker proxy services may mount the real daemon socket. Monitoring uses
  the isolated GET-filtered proxy; AIO lifecycle operations use its separate
  Unix socket proxy. Preserve the documented migration and restart behavior.
- Do not publish secret matches in logs, reports, Markdown, or issue comments.
  Gitleaks runs with complete redaction. Never silently baseline live secrets.

## Documentation and validation

- Read the actual Compose, source, and workflow files before describing them.
- Update the root README and affected component guides after behavior changes.
  Keep Windows PowerShell and macOS/Linux Bash or Zsh examples accurate.
- Prefer generic paths and localhost examples. Distinguish host ports from
  container ports, daemon paths from host paths, and configured from tested behavior.
- Run relevant existing tests, repository privacy checks, and workflow linting.
  Security findings should remain visible; do not weaken CI to make it pass.
- Validate with disposable fixtures and isolated containers. Do not deploy the
  stack or mount the host Docker socket for routine CI or documentation checks.
- Audit tools and dependencies change over time: verify pins and flags against
  primary sources, retain exact action commits/image digests, and document scan limits.
