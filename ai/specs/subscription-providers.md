# Subscription Providers (ACP)

Updated: 2026-03-26

ACP support is a **secondary feature**. Ion's primary path is native ion + canto + direct API. Read `ai/DESIGN.md` first.

---

## Why subscriptions need ACP

For providers that prohibit using subscription OAuth tokens directly (confirmed: Anthropic, Google), ACP is the only compliant path. Ion spawns the official CLI and lets it manage auth — ion never touches credentials.

For other providers (OpenAI, GitHub), OAuth may be permitted for coding tools. ACP is still the right default until we verify this, since it works regardless of ToS posture.

```
subscription user → official CLI (claude/gemini/gh) ←ACP→ ion
```

API key users don't need ACP at all — canto calls the provider API directly.

---

## Provider table

| Provider          | Backend | CLI command        | ToS status                    |
| ----------------- | ------- | ------------------ | ----------------------------- |
| `anthropic`       | canto   | —                  | API key, N/A                  |
| `openai`          | canto   | —                  | API key, N/A                  |
| `openrouter`      | canto   | —                  | API key, N/A                  |
| `claude-pro`      | acp     | `claude --acp`     | Confirmed ACP                 |
| `gemini-advanced` | acp     | `gemini --acp`     | Confirmed ACP                 |
| `gh-copilot`      | acp     | `gh copilot --acp` | Likely OAuth-ok, verify first |
| `chatgpt`         | acp     | `codex --acp`      | Likely OAuth-ok, verify first |
| `codex`           | acp     | `codex --acp`      | CLI agent, verify separately  |

**gh-copilot / chatgpt**: OpenAI and GitHub appear to be supportive of coding tools (OpenCode, ion, etc.) using OAuth for subscription access — but this is unverified. Use ACP as the default for now; it's safe and respects ToS regardless. If OAuth is confirmed, these could route via canto directly (simpler, more features). Do not implement the OAuth path until verified.

**codex**: The local `codex` CLI is the ACP bridge for ChatGPT-style OpenAI subscription access in the current Go implementation. Keep it behind ACP unless and until a native OAuth/API path is verified.

---

## UX

Users pick a provider name. Ion derives the backend silently, but the configured model still belongs to the user.

For the live provider picker, only show providers the current native path can actually use today. Do not show ACP/subscription entries in the picker until that UX is truly ready and usable end to end. ACP/subscription providers can still exist in config/spec logic without being exposed in the picker.

Ion should support the broad set of providers that share the same API families. We only need dedicated verification for providers that materially differ in auth, streaming, cache/reasoning artifacts, or tool-call semantics; the rest can ride the shared backend paths.

```toml
# ~/.ion/config.toml

# Subscription — spawns CLI via ACP, but still needs a model
provider = "claude-pro"
model = "claude-sonnet-4-5"

# API key — calls API directly via canto
provider = "anthropic"
model = "claude-sonnet-4-5"
```

Provider resolution order: `~/.ion/config.toml` → `ION_PROVIDER` env → `--provider` flag.

`ION_ACP_COMMAND` overrides the derived CLI command for custom installs. Does not change backend selection logic.

Model is required for both subscription and API providers. Ion never lets the CLI choose the model implicitly.

---

## ACP feature support

The goal is for ACP mode to feel as seamless as native ion. Users on subscriptions shouldn't hit a wall of missing features.

The mechanism: expose ion-side capabilities (sub-agents, memory, tools) as ACP-callable tools within the session, so the external CLI agent can invoke them. Where the external agent supports a feature natively (e.g. Gemini's experimental sub-agents), surface it through ion's UI. Where it doesn't, ion can fill the gap with its own implementation.

Build native features first, then bridge them to ACP. Do not let ACP work block native ion progress.

Known gaps (tracked in tasks):

- `tk-6zy3` — no token usage from ACP agents
- `tk-o0iw` — no session context (workdir, branch) sent at open
- `tk-2ffy` — agent stderr goes to `os.Stderr`, not session log

Session continuity/resume is still a known ACP gap, but it is not currently tracked as a standalone task.

---

## Startup header

```
# subscription
ion v0.0.0 · ~/project · main
acp · claude-pro · claude-sonnet-4-5

# api key
ion v0.0.0 · ~/project · main
native · anthropic · claude-sonnet-4-5
```
