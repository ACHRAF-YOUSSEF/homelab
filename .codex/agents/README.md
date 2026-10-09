# Reusable global Codex agents

The roles below are personal Codex profiles, installed in the `agents/` directory inside your Codex home. The default location is `~/.codex/agents/` on macOS/Linux and `$env:USERPROFILE\.codex\agents` on Windows. If `CODEX_HOME` is set, use its `agents/` subdirectory instead. These definitions are available across projects on that machine; cloning this repository does not install them.

Read [AGENTS.md](../../AGENTS.md) for homelab-specific privacy, Docker access, documentation, and validation instructions. Keep those rules in the repository so personal agents can work on unrelated projects too.

| Agent | Model and effort | Use |
| --- | --- | --- |
| `repo_explorer` | `gpt-6-luna`, medium | Read-only repository exploration and concise findings. |
| `docs_maintainer` | `gpt-6-luna`, medium | Maintain focused project documentation. |
| `validation_runner` | `gpt-6-luna`, medium | Run requested validation and report results. |
| `security_reviewer` | `gpt-6.1-sol`, high | Read-only security review of code and configuration. |

Keep routine exploration, documentation, and validation on the smaller model. Reserve the stronger security profile for work that needs deeper security judgment. Each personal profile selects its own model and effort; it does not change the primary agent's model. The repository's [.codex/config.toml](../config.toml) still supplies a smaller default for other subagents and limits concurrent child agents to three while working in this project.

## Global setup

Create the personal agent directory in Windows PowerShell:

```powershell
$codexDirectory = if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }
$agentDirectory = Join-Path $codexDirectory 'agents'
New-Item -ItemType Directory -Force -Path $agentDirectory | Out-Null
```

Or in macOS/Linux Bash or Zsh:

```bash
agent_directory="${CODEX_HOME:-$HOME/.codex}/agents"
mkdir -p "$agent_directory"
```

Save each definition as a standalone TOML file in that directory, preferably with the filename matching its `name`. Each file requires `name`, `description`, and `developer_instructions`; it can also select `model`, `model_reasoning_effort`, and `sandbox_mode`. For example, `repo_explorer.toml` can contain:

```toml
name = "repo_explorer"
description = "Read-only repository exploration and evidence gathering."
model = "gpt-6-luna"
model_reasoning_effort = "medium"
sandbox_mode = "read-only"
developer_instructions = """
Read applicable project guidance, including AGENTS.md when present.
Use targeted searches and reads to trace the assigned files and dependencies.
Report concrete file references, observed facts, and unresolved questions.
Do not edit files or expose secrets or private configuration values.
Remain available for follow-up work in the same session.
"""
```

Use models available to your account. Select `workspace-write` for the documentation and validation roles, and `read-only` for exploration and security review. Keep each role's instructions focused on its responsibility and applicable project guidance. Personal profiles should use templates when inspecting configuration and keep sensitive values out of reports.

You can add role-selection and reuse preferences to `AGENTS.md` inside your Codex home. An active global `AGENTS.override.md` takes precedence, so put the preferences in the applicable global guidance file and preserve its existing instructions. Global instructions apply across projects; project `AGENTS.md` files add the repository-specific rules.

The repository deliberately contains no project-scoped TOML role definitions. Its `.codex/agents/` directory contains this setup guide only, while the installed definitions live in your personal Codex home. See the [official subagent documentation](https://learn.chatgpt.com/docs/agent-configuration/subagents) and [global AGENTS.md guidance](https://learn.chatgpt.com/docs/agent-configuration/agents-md) for loading behavior and configuration details.

## Reuse

In a chat, ask: "Use `repo_explorer` to trace the Docker API proxy configuration and report the relevant files." For a later step in the same chat, ask the existing specialist to follow up. Start a new Codex session after installing or changing personal definitions or global guidance so the session loads the updated configuration.

The saved role instructions are reusable across projects and chats. An agent's conversation history belongs to its current session. If a personal role is unavailable on another machine, use an available agent with the same scoped assignment and follow the repository guidance.
