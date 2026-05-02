# Subscription Providers (ACP)

Updated: 2026-05-02

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
| `chatgpt`         | deferred | —                 | Official Codex surfaces only  |
| `codex`           | deferred | —                 | App-server/MCP, not ACP       |

**gh-copilot**: GitHub appears to support coding-tool OAuth access through
official GitHub/Copilot surfaces, but Ion should still verify the current
protocol and terms before exposing this.

**chatgpt / codex**: Do not treat Codex as ACP. Current official OpenAI
surfaces are Codex CLI/IDE/app/web with ChatGPT sign-in or API-key sign-in, plus
Codex app-server and MCP surfaces. There is no supported `codex --acp` command
in the current CLI. Ion must not scrape or reuse ChatGPT OAuth tokens directly.

Future ChatGPT-subscription support, if it is worth doing, should be a separate
Codex app-server adapter or host integration that controls Codex through its
documented protocol. That would be a secondary compatibility bridge, not
Ion's native Canto backend and not an Apps SDK integration.

The OpenAI Apps SDK is for building apps inside ChatGPT and public app/plugin
distribution. It does not provide a model-provider backend that lets Ion spend a
user's ChatGPT subscription from Ion's own loop.

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

`ION_ACP_COMMAND` overrides the derived CLI command for custom installs. It
does not change backend selection logic and should not be used to point Ion at
Codex unless Ion has a Codex app-server adapter.

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

## ChatGPT / Codex evaluation snapshot

Current evidence:

- OpenAI's Codex CLI docs say Codex CLI runs locally and first-run auth prompts
  for either a ChatGPT account or an API key:
  <https://developers.openai.com/codex/cli>.
- OpenAI's help center says ChatGPT Plus, Pro, Business, Edu, and Enterprise
  plans include Codex, with plan-specific data controls:
  <https://help.openai.com/en/articles/11369540-using-codex-with-your-chatgpt-plan>.
- OpenAI's Codex changelog documents ChatGPT sign-in model availability and
  app-server login/device-code work, which confirms the protocol is Codex
  app-server specific rather than ACP:
  <https://developers.openai.com/codex/changelog>.
- OpenAI's Apps SDK docs describe ChatGPT apps/plugins and Codex plugin
  distribution, not third-party CLI backend access to a ChatGPT subscription:
  <https://developers.openai.com/apps-sdk>.

Ion decision:

- Keep `chatgpt` and `codex` catalog entries hidden/deferred.
- Do not derive a default command for them.
- Prefer native OpenAI API keys for Ion's Canto path.
- Treat Codex app-server integration as a later bridge evaluation, only after
  current I4 table-stakes work is done.

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
