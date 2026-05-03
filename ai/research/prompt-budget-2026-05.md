---
date: 2026-05-02
summary: Baseline measurement for Ion static prompt size and prompt-cache priority
status: active
---

# Prompt Budget Baseline

**Status:** reference, 2026-05-02
**Scope:** static Ion/Canto prelude size, reference-agent comparison, and prompt-cache priority.

## Result

Ion's always-on native prompt is currently small enough for P1 stabilization. The biggest static cost in this repository is project instructions, not Ion's core prompt or the eight P1 tool specs.

Measured with `TestPromptPreludeBudgetReport` in `internal/backend/canto`:

| Segment | Size |
| --- | ---: |
| Core Ion prompt | 1,848 chars, about 462 tokens |
| Runtime prompt | 135 chars, about 34 tokens |
| Core + runtime | 1,985 chars, about 497 tokens |
| Project instruction layers | 8,437 chars, about 2,110 tokens |
| P1 tool specs | 3,841 chars, about 961 tokens |
| Current static total | 14,263 chars, about 3,566 tokens |

Token counts are rough `chars / 4` estimates. They are useful for budget tracking, not provider billing.

## Reference measurements

Local Pi prompt measurements from `/Users/nick/github/badlogic/pi-mono/packages/coding-agent`:

| Reference | Size |
| --- | ---: |
| Pi prompt with read/bash/edit/write snippets | 1,636 chars, about 409 tokens |
| Pi prompt with read/bash/edit/write/grep/find/ls snippets | 1,755 chars, about 439 tokens |

Local OpenCode prompt file sizes from `/Users/nick/github/anomalyco/opencode/packages/opencode/src/session/prompt`:

| Prompt | Size |
| --- | ---: |
| `default.txt` | 8,661 chars, about 2,166 tokens |
| `anthropic.txt` | 8,212 chars, about 2,053 tokens |
| `gpt.txt` | 9,284 chars, about 2,321 tokens |
| `codex.txt` | 7,390 chars, about 1,848 tokens |
| `beast.txt` | 11,080 chars, about 2,770 tokens |
| `copilot-gpt-5.txt` | 14,240 chars, about 3,560 tokens |
| `gemini.txt` | 15,372 chars, about 3,843 tokens |

The user-provided Antirez/OpenCode note is directionally useful: long static prompts create latency and cache-pressure concerns, and Pi's small prompt is a good sanity check. The exact token count depends on tokenizer and which OpenCode prompt/tools/plugins/config are active.

## Interpretation

- Ion's base prompt is not bloated. It is larger than Pi's minimal prompt, but still small.
- The eight P1 tool specs are under about 1k estimated tokens, which supports keeping the current small native tool surface stable.
- The repo's project instruction layer is the large static component in this measurement. That is expected in this workspace because `AGENTS.md` carries project-wide policy, architecture, and workflow instructions.
- Ion should not implement disk KV or repeated-prefix prompt caching in this pass. Prompt caching is provider/runtime-owned unless Ion owns the local inference server. If Ion later owns the serving stack, cache only stable repeated prefixes with clear invalidation and a sensible TTL.

## Direction

Keep the P1 work simple:

- Maintain the eight-tool surface: `bash`, `read`, `write`, `edit`, `multi_edit`, `list`, `grep`, `glob`.
- Avoid adding default model-visible tools until there is measured value.
- Keep tool descriptions concise and aligned with `docs/tools.md`.
- Treat project-instruction growth as an `ai/` and `AGENTS.md` hygiene problem, not an Ion base-prompt problem.
- Do not add prompt-cache machinery during core-loop stabilization.

## Follow-up

Keep `TestPromptPreludeBudgetReport` as a cheap regression report. If future work adds tools, memory, subagents, MCP, policy, or long formatting manuals to the default prompt, run the report and update this note before accepting the growth.
