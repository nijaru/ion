# Codex CLI: System Prompt, Tools, and Agent Loop Analysis

**Date:** 2026-02-14 (updated from 2026-02-09)
**Source:** github.com/openai/codex (open source, codex-rs Rust implementation)
**Focus:** What makes Codex CLI effective with gpt-5.3-codex vs. third-party agents

## Key Findings Summary

| Aspect             | Codex CLI                                                          | ion                           |
| ------------------ | ------------------------------------------------------------------ | ----------------------------- |
| API                | Responses API (WebSocket + SSE)                                    | Chat Completions API          |
| Tool format        | `local_shell` + `apply_patch` (freeform custom type)               | Standard function tools       |
| Tool result format | Structured text (exit code, duration, output)                      | Raw output                    |
| System prompt      | Model-specific, ~4000 words for gpt-5.3-codex                      | Generic, ~500 words           |
| Agent loop         | `needs_follow_up` flag from tool calls, no iteration cap           | `tool_calls.is_empty()` check |
| Compaction         | Token-limit-triggered auto-compact mid-turn                        | Threshold-based between turns |
| Env context        | XML-serialized `<environment_context>` block                       | Plain text in system prompt   |
| Model config       | Per-model JSON with shell_type, apply_patch_type, reasoning levels | None                          |

## 1. System Prompts -- Model-Specific Design

Codex CLI uses **different system prompts per model family**, loaded at runtime:

| File                                      | Model                             |
| ----------------------------------------- | --------------------------------- |
| `prompt.md`                               | Default (generic models)          |
| `prompt_with_apply_patch_instructions.md` | Models with apply_patch via shell |
| `gpt_5_codex_prompt.md`                   | gpt-5-codex                       |
| `gpt_5_1_prompt.md`                       | gpt-5.1                           |
| `gpt_5_2_prompt.md`                       | gpt-5.2                           |
| `gpt-5.2-codex_prompt.md`                 | gpt-5.2-codex                     |
| `gpt-5.1-codex-max_prompt.md`             | gpt-5.1-codex-max                 |

For **gpt-5.3-codex** specifically, the system prompt is embedded directly in `models.json` as `base_instructions` -- a massive ~4000-word prompt stored per-model-config. This is the most sophisticated prompt.

### Personality System (gpt-5.3-codex unique)

The prompt supports templated personality variants via `{{ personality }}`:

- **pragmatic** (default): "deeply pragmatic, effective software engineer"
- **friendly**: "optimize for team morale and being a supportive teammate"

Injected into the base instructions template at runtime.

### Key Behavioral Instructions (gpt-5.3-codex)

- "Persist until the task is fully handled end-to-end within the current turn"
- "Unless the user explicitly asks for a plan... assume the user wants you to make code changes"
- "You must keep going until the query or task is completely resolved"
- "Persevere even when function calls fail"
- Explicit instructions about commentary channel vs final channel
- "You provide user updates frequently, every 20s"
- "Parallelize tool calls whenever possible - especially file reads. Use `multi_tool_use.parallel`"

### Critical Difference: Autonomy Language

The gpt-5.1+ prompts add an entire "Autonomy and Persistence" section absent from the generic prompt:

> "Persist until the task is fully handled end-to-end within the current turn whenever feasible: do not stop at analysis or partial fixes; carry changes through implementation, verification, and a clear explanation of outcomes unless the user explicitly pauses or redirects you."

The generic `prompt.md` says "Please keep going" -- the codex prompts say "You **must** keep going" and "persevere even when function calls fail."

### Prompt Assembly

System instructions are sent as the `instructions` field in OpenAI's Responses API:

```rust
pub struct ResponsesApiRequest<'a> {
    pub model: &'a str,
    pub instructions: &'a str,  // system prompt
    pub input: &'a [ResponseItem],  // conversation history
    pub tools: &'a [serde_json::Value],
    pub tool_choice: &'static str,
    pub parallel_tool_calls: bool,
    pub reasoning: Option<Reasoning>,
    pub stream: bool,
    pub text: Option<TextControls>,  // verbosity control
}
```

Priority for base instructions:

1. `config.base_instructions` override (user config)
2. `conversation_history.get_base_instructions()` (session state)
3. Model-specific default from `ModelInfo`

### Additional Context Layers (injected as user messages)

- **Environment context**: XML-formatted `<environment_context>` with cwd, shell, network info
- **AGENTS.md instructions**: `# AGENTS.md instructions for {directory}\n\n<INSTRUCTIONS>\n{contents}\n</INSTRUCTIONS>`
- **Skill instructions**: `<skill>\n<name>{name}</name>\n<path>{path}</path>\n{contents}\n</skill>`
- **Collaboration mode**: Templates appended for plan/execute/pair-programming/default modes

### Collaboration Modes

- **Default** (680 bytes): Minimal, strongly prefers executing over asking
- **Execute** (3.9KB): Independent execution, assumptions-first
- **Plan** (7KB): 3-phase planning, strict no-mutation rules
- **Pair Programming** (1.1KB): Build together, small steps

## 2. Tool Definitions -- Not Standard Function Calling

Codex CLI uses the **OpenAI Responses API**, not Chat Completions. Tools are a mix of types:

### Tool Types (from `client_common.rs`)

```rust
enum ToolSpec {
    Function(ResponsesApiTool),   // Standard JSON schema function
    LocalShell {},                // Native shell execution (no schema)
    WebSearch { external_web_access: Option<bool> },
    Freeform(FreeformTool),       // Custom format tools
}
```

### The `local_shell` Tool

A **first-class Responses API type** -- not a function tool. No JSON schema. The model natively understands how to invoke shell commands through `LocalShellCall` response items. This is an OpenAI-specific capability baked into the model.

### The `apply_patch` Freeform Tool

For gpt-5.x-codex models, file editing uses a **freeform custom tool** -- not JSON function calling:

```rust
struct FreeformTool {
    name: String,           // "apply_patch"
    description: String,
    format: FreeformToolFormat {
        r#type: String,     // "freeform"
        syntax: String,     // The patch format grammar
        definition: String, // Full BNF-like grammar definition
    },
}
```

The model outputs raw patch text directly (not JSON-wrapped), parsed by a custom patch engine. Avoids JSON serialization overhead and escaping issues with code content.

### Shell Tool Variants (per-model in `models.json`)

- `shell_command`: Simple string command (gpt-5.x-codex models)
- `shell`: Array-based command (older models)
- `unified_exec`: PTY-based execution with session management (newer experimental)

For gpt-5.3-codex: `"shell_type": "shell_command"` -- the simplest variant.

### Core Tool Set

Always present: shell (variant depends on model), `update_plan`, MCP resource tools.
Conditional: `apply_patch`, `request_user_input`, `search_tool_bm25`, `grep_files`, `read_file`, `list_dir`, `view_image`, `web_search`, sub-agent tools (spawn/send/wait/resume/close).

Typical tool count: **6-10 core** + MCP tools. Up to ~16 with all features.

### Tool Description Verbosity

All property descriptions are present and human-readable. No abbreviation.

- `additionalProperties: false` on every object (helps model adherence)
- `strict: false` on all tools (allows flexible model output)

Main optimization is **conditional tool inclusion**, not minification.

## 3. Tool Result Formatting

### Structured Shell Output

Shell outputs are **always** wrapped with metadata:

```
Exit code: 0
Wall time: 1.2 seconds
Total output lines: 42
Output:
<actual output, truncated to token limit>
```

This is from `format_exec_output_for_model_freeform()` in `tools/mod.rs`. The `reserialize_shell_outputs` function in `client_common.rs` converts JSON tool outputs into this structured text when the freeform apply_patch tool is present.

### Truncation Policy

From `models.json` for gpt-5.3-codex:

```json
"truncation_policy": { "mode": "tokens", "limit": 10000 }
```

Large outputs are truncated to 10k tokens per tool result, preventing single tool outputs from dominating context.

## 4. Agent Loop -- How It Decides to Continue

### Core Loop (from `codex.rs:run_turn`, line 4203)

```
loop {
    // 1. Drain any pending user messages (mid-turn steering)
    // 2. Build full history for prompt
    // 3. Stream model response via run_sampling_request
    // 4. For each OutputItemDone:
    //    - If tool call: queue execution future, set needs_follow_up = true
    //    - If message: record as last_agent_message
    //    - If error: still set needs_follow_up = true
    // 5. On Completed event:
    //    - needs_follow_up |= has_pending_input
    //    - If token_limit_reached AND needs_follow_up: auto-compact, continue
    //    - If !needs_follow_up: break (model decided to stop)
    //    - Otherwise: continue
}
```

### Key Design: `needs_follow_up` Flag

The loop continues when **any** of these are true:

1. The model made tool calls (`needs_follow_up` set by `handle_output_item_done`)
2. A tool error was returned to the model (even missing call IDs trigger follow-up)
3. The user submitted input mid-turn (`has_pending_input`)
4. The model's response was interrupted and needs retry

**No visible max-turns/iterations limit.** The model decides when to stop by not making tool calls.

### Mid-Turn Auto-Compaction

When total token usage exceeds `auto_compact_token_limit` AND the turn needs follow-up, Codex runs compaction inline and continues. Prevents context window exhaustion during long agent runs.

### Parallel Tool Execution

Tool calls within a single response are executed via `FuturesOrdered`:

```rust
let mut in_flight: FuturesOrdered<BoxFuture<'static, CodexResult<ResponseInputItem>>> =
    FuturesOrdered::new();
// ... for each tool call:
in_flight.push_back(tool_future);
```

Runs in parallel but results collected in order.

### Retry and Transport Fallback

Streaming failures trigger retries with exponential backoff. After exhausting retry budget, the client falls back from WebSocket to HTTPS SSE transport and resets the retry counter.

## 5. Model-Specific Configuration (gpt-5.3-codex)

From `models.json`:

```json
{
  "slug": "gpt-5.3-codex",
  "context_window": 272000,
  "supports_parallel_tool_calls": true,
  "apply_patch_tool_type": "freeform",
  "shell_type": "shell_command",
  "default_reasoning_level": "medium",
  "supported_reasoning_levels": ["low", "medium", "high", "xhigh"],
  "supports_reasoning_summaries": true,
  "support_verbosity": true,
  "default_verbosity": "low",
  "reasoning_summary_format": "experimental",
  "truncation_policy": { "mode": "tokens", "limit": 10000 }
}
```

Key model-specific features:

- **Verbosity control**: `text.verbosity` parameter set to "low" by default
- **Reasoning summaries**: Model reasoning traces summarized for display
- **272k context window**: Enables very long agent runs
- **xhigh reasoning**: Fourth tier beyond "high" for maximum depth
- **Truncation at 10k tokens per tool result**

## 6. Environment Context

Codex injects environment context as XML:

```xml
<environment_context>
  <cwd>/path/to/project</cwd>
  <shell>bash</shell>
</environment_context>
```

Injected as a system/developer message item, separate from the system prompt. Updated between turns when cwd or network config changes.

## 7. Actionable Gaps: What ion is Missing

### High Impact (likely explains performance gap)

1. **Responses API for OpenAI models**: The `local_shell` tool type and freeform `apply_patch` are Responses API features that gpt-5.x-codex models were trained on. Using Chat Completions API means the model must work with a format it wasn't optimized for. This is probably the single biggest factor.

2. **Structured tool results**: ion returns raw tool output. Codex wraps every shell result with exit code, duration, and line count metadata. This gives the model better signal about success/failure without parsing.

3. **Dramatically more aggressive autonomy language**: ion's prompt says "Keep going until the task is fully resolved" (1 sentence). Codex's gpt-5.3 prompt has an entire "Autonomy and Persistence" section (~150 words) repeated in multiple phrasings: "You must keep going", "Persevere even when function calls fail", "do not stop at analysis or partial fixes; carry changes through implementation, verification." The repetition and intensity matter for model behavior.

4. **Tool output truncation**: Codex truncates tool results to 10k tokens. Large outputs (full test suite dumps, big file reads) can poison ion's context and cause the model to lose track.

### Medium Impact

5. **Model-specific prompts**: Codex tailors prompts per model family. Different models get different system prompts, tool types, and behavioral instructions. ion uses the same prompt for all models.

6. **Mid-turn auto-compaction**: Codex compacts the conversation mid-turn if tokens exceed the limit, then continues. ion compacts between turns only. For long agent runs, this prevents hard failures.

7. **Parallel tool call instruction**: The gpt-5.3 prompt explicitly tells the model "Parallelize tool calls whenever possible - especially file reads. Use `multi_tool_use.parallel`." ion mentions parallel calls but less emphatically.

8. **Freeform apply_patch**: The custom patch format avoids JSON escaping issues with code. When code contains quotes, backslashes, or newlines, JSON function calling can produce malformed tool calls. The freeform format sidesteps this entirely.

### Lower Impact

9. **Personality system**: Configurable personality (pragmatic vs friendly) is a UX feature, unlikely to affect task completion.

10. **Progress update instructions**: The gpt-5.3 prompt includes detailed instructions about commentary/progress updates every 20 seconds. UX quality-of-life.

11. **AGENTS.md scoping spec**: Codex has a detailed spec for AGENTS.md scoping rules (directory-tree scope, precedence). ion loads AGENTS.md but may not implement the full scoping semantics.

## 8. Concrete Recommendations for ion (Priority Order)

1. **Investigate Responses API for OpenAI provider** -- the `local_shell` native tool type and freeform tools likely improve model performance since gpt-5.x-codex was trained on these formats. This requires a new provider implementation path.

2. **Enrich system prompt autonomy language** -- adopt the aggressive persistence language from gpt-5.3-codex prompt. The key phrases: "must keep going", "persevere even when function calls fail", "do not stop at analysis or partial fixes; carry changes through implementation, verification". Quick change, testable immediately.

3. **Add structured tool result metadata** -- wrap shell output with exit code, duration, total lines. Format: `Exit code: N\nWall time: Xs\nOutput:\n...`. ~20 lines of code change in tool result formatting.

4. **Add truncation policy for tool outputs** -- cap at ~10k tokens per tool result. Large outputs currently flood the context.

5. **Model-specific prompt selection** -- at minimum, detect gpt-5.x models and use a more aggressive/specific prompt. The prompts are already available as reference in this analysis.

6. **Mid-turn compaction** -- when token count exceeds threshold during an active tool loop, compact before the next model call rather than failing or waiting for the loop to end.
