# Reusable Codex agents

This repository keeps small, task-focused agent profiles in `.codex/agents/` and shared defaults in [.codex/config.toml](../config.toml). Read [AGENTS.md](../../AGENTS.md) for repository-wide instructions.

| Agent | Model and effort | Use |
| --- | --- | --- |
| `repo_explorer` | `gpt-6-luna`, medium | Read-only repository exploration and concise findings. |
| `docs_maintainer` | `gpt-6-luna`, medium | Maintain focused project documentation. |
| `validation_runner` | `gpt-6-luna`, medium | Run requested validation and report results. |
| `security_reviewer` | `gpt-6.1-sol`, high | Read-only security review of code and configuration. |

Keep routine exploration and documentation work on the smaller model. Reserve the stronger security profile for higher judgment review. The shared configuration uses `gpt-6-luna` at medium effort by default and limits concurrent child agents to three. Review the concrete profiles: [repo_explorer](repo_explorer.toml), [docs_maintainer](docs_maintainer.toml), [validation_runner](validation_runner.toml), and [security_reviewer](security_reviewer.toml).

Codex reads standalone TOML profiles from `.codex/agents/`. Each profile requires `name`, `description`, and `developer_instructions`; `model` and other settings are optional. See the [official subagent configuration documentation](https://learn.chatgpt.com/docs/agent-configuration/subagents) for the current format and behavior.

For a follow-up in the current chat, ask: “Use the `repo_explorer` profile to trace how the Docker API proxy is configured and report the relevant files.” In a later chat, ask Codex to load `.codex/agents/repo_explorer.toml` and use that role for the same task; the saved file provides the profile instructions for that new chat.

