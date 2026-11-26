# Configuration

For basic Web2Codex configuration instructions, see the upstream [OpenAI Codex basic configuration reference](https://developers.openai.com/codex/config-basic).

For advanced Web2Codex configuration instructions, see the upstream [OpenAI Codex advanced configuration reference](https://developers.openai.com/codex/config-advanced).

For a full Web2Codex configuration reference, see the upstream [OpenAI Codex configuration reference](https://developers.openai.com/codex/config-reference).

## Lifecycle hooks

Admins can set top-level `allow_managed_hooks_only = true` in
`requirements.toml` to ignore user, project, and session hook configs while
still allowing managed hooks from requirements and managed config layers. This
setting is only supported in `requirements.toml`; putting it in `config.toml`
does not enable managed-hooks-only mode.
