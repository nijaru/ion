---
date: 2026-04-27
summary: Provider thinking/reasoning controls and recommended Ion capability model.
status: active
---

# Thinking Effort Provider Survey

## Bottom Line

Ion should expose one compact thinking control and translate it per provider/model.
There is no universal budget API:

- Some providers use named effort levels.
- Some providers use numeric thinking budgets.
- Some providers only expose binary thinking on/off or reasoning model IDs.
- OpenAI-compatible custom endpoints often reject unknown thinking parameters.

The TUI should show only supported values for the selected model. Raw numeric
budgets belong in config/model capability overrides, not the normal TUI.

## Recommended Ion Vocabulary

User-facing levels:

| Ion value | Meaning |
| --- | --- |
| `auto` | Send no override; use the provider/model default. |
| `off` | Explicitly disable thinking when the model supports disabling. |
| `low` | Favor speed/cost. |
| `medium` | Balanced effort. |
| `high` | Deep effort for most serious coding tasks. |
| `xhigh` | Extra-high effort where a model explicitly supports it. |
| `max` | Provider-specific maximum only where explicitly supported; session-oriented UX. |

Rules:

- Do not send unsupported reasoning parameters to unknown/custom endpoints.
- If a selected level becomes unsupported after model switch, fall back to the
  highest supported level at or below the selected value and show a short notice.
- `auto` is the safest default and means provider default, not Ion planner magic.
- `max` should not be treated as a portable top value; many models do not have it.

## Provider Findings

| Provider/API | Control shape | Practical Ion handling |
| --- | --- | --- |
| OpenAI Responses | `reasoning.effort`, model-specific values including `none`/`minimal`/`low`/`medium`/`high`/`xhigh` on current reasoning models. | Use named efforts from model metadata. Map Ion `off` to `none` when supported. Do not invent `max`. |
| Anthropic Claude | New models prefer adaptive thinking with `output_config.effort`; supported values vary by model. Older/manual mode uses `thinking.budget_tokens`. | Prefer adaptive named effort for models that support it. Keep numeric budget as adapter/config detail for older models. |
| Gemini | Gemini 3 uses `thinkingLevel`; Gemini 2.5 uses numeric `thinkingBudget`; some models cannot disable thinking. | Treat Gemini 3 as named levels and Gemini 2.5 as budget-backed levels. Hide `off` when disable is unavailable. |
| OpenRouter | Offers a cross-provider `reasoning` object with `effort`, `max_tokens`, and `exclude`, with provider-specific mapping. | Good reference abstraction for OpenRouter only; direct APIs still need native adapters. |
| xAI | Reasoning support is model-specific; docs say some current Grok models do not support `reasoning_effort`. | Capability table only; no blanket xAI reasoning param. |
| Mistral | `mistral-small-latest` documents `reasoning_effort`, currently `high` and `none`. | Expose only documented values for that model. |
| DeepSeek | Thinking mode can be enabled by model selection or `thinking` parameter and returns `reasoning_content`; no general low/medium/high budget. | Treat as binary/model-mode. Preserve provider-specific reasoning content semantics. |
| Qwen/Alibaba | DashScope/OpenAI-compatible APIs expose `enable_thinking`; Qwen docs describe thinking budget support on hosted APIs, while local/open-source serving varies. | For hosted Qwen, support binary and optional numeric budget mapping. For local llama.cpp/vLLM, require custom capability config before sending params. |
| Unknown/custom endpoint | Usually OpenAI-compatible but may reject nonstandard fields. | Default to `auto` with no reasoning fields. Allow explicit per-model capability override. |

## Custom Endpoint Config Direction

Support optional per-model capability overrides in `~/.ion/config.toml` or a
future model capability file. Keep the initial shape small:

```toml
[model_capabilities."local-api:qwen3.6:27b"]
thinking = "budget" # none | effort | budget | boolean
levels = ["off", "low", "medium", "high"]
default = "auto"
budgets = { low = 1024, medium = 4096, high = 8192 }
```

Notes:

- This is stable user config, not mutable state.
- The TUI should consume the capability result and show levels, not raw numbers.
- Raw numeric budget editing can be config-only until there is evidence users
  need frequent runtime tuning.

## Competitor Patterns

- Claude Code exposes `/effort`, model-aware supported levels, and downward
  fallback when a selected effort is unsupported by the active model. It also
  treats `max` as a special high-spend/session-oriented value.
- Pi persists a thinking level, clamps it to model capabilities, and supports
  custom thinking budgets in lower-level agent options. It also handles
  cross-provider thinking-block transformations.
- Codex/OpenAI surfaces reasoning effort around OpenAI model capabilities; Ion
  should copy the capability-driven behavior, not hardcode OpenAI's newest enum
  as a universal set.

## Implementation Implications

- Canto should own provider request translation: named effort, numeric budget,
  boolean thinking, and provider-specific reasoning content handling.
- Ion should own UI, settings/state, and the effective-level display.
- Current Ion normalization is too narrow because it only accepts
  `auto|low|medium|high`.
- The thinking picker should be capability filtered. `Ctrl+T` should open a
  selector rather than blindly cycling unsupported values.

## Sources

- OpenAI reasoning/model docs: https://platform.openai.com/docs/guides/reasoning
- OpenAI GPT-5.2 model page: https://platform.openai.com/docs/models/gpt-5.2/
- Anthropic adaptive thinking: https://platform.claude.com/docs/en/build-with-claude/adaptive-thinking
- Anthropic effort docs: https://platform.claude.com/docs/en/build-with-claude/effort
- Claude Code model config: https://code.claude.com/docs/en/model-config
- Claude Code commands: https://code.claude.com/docs/en/commands
- Gemini thinking docs: https://ai.google.dev/gemini-api/docs/thinking
- OpenRouter reasoning tokens: https://openrouter.ai/docs/guides/best-practices/reasoning-tokens
- xAI reasoning docs: https://docs.x.ai/developers/model-capabilities/text/reasoning
- Mistral adjustable reasoning: https://docs.mistral.ai/capabilities/reasoning/adjustable
- DeepSeek thinking mode: https://api-docs.deepseek.com/guides/thinking_mode
- Alibaba/Qwen deep thinking: https://www.alibabacloud.com/help/en/model-studio/deep-thinking
- Qwen thinking budget docs: https://qwen.readthedocs.io/en/v3.0/getting_started/quickstart.html
- Pi changelog: https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/CHANGELOG.md
