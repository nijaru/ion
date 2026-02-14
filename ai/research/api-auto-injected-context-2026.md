# LLM API Auto-Injected Context Research

**Date:** 2026-02-14
**Purpose:** What system/environment information do major LLM APIs automatically inject into model context, without developer control?

## Summary Table

| Field              | Anthropic API | OpenAI API (GPT-5) | Google Gemini API | DeepSeek API |
|--------------------|---------------|---------------------|-------------------|--------------|
| Current date       | No            | Yes                 | No                | No           |
| Knowledge cutoff   | No            | Yes                 | No                | No (in system prompt, static) |
| OS/platform        | No            | No                  | No                | No           |
| Working directory  | No            | No                  | No                | No           |
| Shell type         | No            | No                  | No                | No           |
| Formatting hints   | No            | Yes                 | No                | No           |
| Oververbosity      | No            | Yes                 | No                | No           |
| Internal params    | No            | Yes ("Juice")       | No                | No           |

**Key finding:** OpenAI is the only provider that auto-injects context into raw API calls. All others leave the system prompt entirely under developer control.

## Detailed Findings

### Anthropic Claude API

**Auto-injected into API:** Nothing.

Anthropic explicitly states on their system prompt documentation page:

> "These system prompt updates do not apply to the Anthropic API."

The system prompts published by Anthropic (which include current date, formatting guidance, tool instructions) are only used by Claude.ai web/mobile apps. When using the API directly, the developer has full control -- no hidden preamble, no date injection, no metadata.

**Implication for ion:** We must inject current date, working directory, and all environment context ourselves. This is exactly what `context.rs` already does via the template system.

**Source:** https://platform.claude.com/docs/en/release-notes/system-prompts

### OpenAI API (GPT-5 / Chat Completions)

**Auto-injected into API:** Yes, significant hidden system prompt.

OpenAI injects a hidden preamble into every GPT-5 API call that the developer cannot override or disable. The extracted content (via prompt injection / leaked prompts, Aug-Nov 2025):

```
Current date: 2025-08-15
Knowledge cutoff: 2024-10

You are an AI assistant accessed via an API. Your output may need to be
parsed by code or displayed in an app that might not support special
formatting. Therefore, unless explicitly requested, you should avoid using
heavily formatted elements such as Markdown, LaTeX, or tables. Bullet
lists are acceptable.

Desired oververbosity for the final answer (not analysis): 3

An oververbosity of 1 means the model should respond using only the minimal
content necessary to satisfy the request...
An oververbosity of 10 means the model should provide maximally detailed...

The desired oververbosity should be treated only as a *default*.
Defer to any user or developer requirements regarding response length.

Valid channels: analysis, commentary, final.
Channel must be included for every message.

Juice: 64
```

**Fields injected:**
1. **Current date** -- dynamic, updates daily
2. **Knowledge cutoff** -- static per model version
3. **API context notice** -- tells model it's accessed via API
4. **Formatting constraints** -- discourages Markdown/LaTeX/tables (prefers plain text)
5. **Oververbosity** -- default response length setting (1-10 scale, default 3)
6. **Channels** -- internal output routing (analysis/commentary/final)
7. **Juice** -- suspected compute/effort budget parameter (undocumented)

**Developer complaints:** This injection conflicts with developer use cases -- e.g., the formatting guidance suppresses Markdown even when developers want it. The date injection can also conflict with developer-provided dates. Multiple forum threads requesting OpenAI stop this practice.

**Implication for ion:** When using OpenAI models, the provider already injects the date, but it may also inject unwanted formatting constraints. Developer system prompts can partially override (the oververbosity/formatting are stated as defaults), but the hidden prompt cannot be fully removed.

**Sources:**
- https://community.openai.com/t/to-openai-you-must-stop-injecting-a-system-message-to-api-gpt-5-that-is-counter-to-developer-applications/1348819
- https://simonwillison.net/2025/Aug/15/gpt-5-has-a-hidden-system-prompt
- https://aiengineerguide.com/blog/openai-gpt-5-api-hidden-prompt/
- https://shinobi.security/blog/gpt5-context-poisoning

### Google Gemini API

**Auto-injected into API:** Nothing confirmed.

Community testing confirms Gemini API does not auto-inject the current date. A Google AI Developer Forum thread (Dec 2025) specifically asked this question, and the answer was confirmed: Gemini does not natively know the current date via API.

When tested with date-dependent questions, Gemini models hallucinated day-of-week calculations, confirming no real-time date awareness. The Gemini web app (gemini.google.com) does inject date/context via its own system prompt, but the raw API does not.

**Implication for ion:** Must inject all environment context when using Gemini models.

**Source:** https://discuss.ai.google.dev/t/does-the-gemini-api-natively-know-the-current-date-or-must-it-be-injected-via-system-instructions/111957

### DeepSeek API

**Auto-injected into API:** Nothing dynamic.

DeepSeek's system prompt (extracted via prompt injection, documented by Knostic) contains only static identity and cutoff info:

> "You are DeepSeek-V3, an artificial intelligence assistant created by DeepSeek. Your knowledge cutoff is July 2024..."

No dynamic date injection, no environment metadata, no formatting guidance. The knowledge cutoff is baked into the static default system prompt, not dynamically injected.

**Implication for ion:** Must inject all environment context when using DeepSeek models.

**Sources:**
- https://www.knostic.ai/blog/exposing-deepseek-system-prompts
- https://github.com/deepseek-ai/DeepSeek-V3/issues/1047

## Relevance to ion

ion's `src/agent/context.rs` already handles this correctly by injecting:
- **Working directory** via template `{{ working_dir }}`
- **Current date** via `chrono::Local::now()` into `{{ date }}`

This is necessary for Anthropic, Gemini, and DeepSeek (no auto-injection). For OpenAI, the date is redundant but harmless -- having it in our system prompt reinforces accuracy.

### What ion does NOT inject (but could consider)

- **OS/platform** -- no provider auto-injects this. Could be useful for bash tool guidance.
- **Shell type** -- no provider auto-injects this. Relevant for command syntax.
- **Git branch** -- sometimes useful for context.
- **Time of day** -- only date is provided currently, not time.

### Recommendation

The current approach of always injecting date + working_dir is correct. No provider reliably auto-injects these fields, and even OpenAI's injection shouldn't be relied upon since it's undocumented and could change.

Consider adding OS and shell info to the environment section for better bash command generation, similar to how Claude Code's system prompt includes platform/shell/OS info.
