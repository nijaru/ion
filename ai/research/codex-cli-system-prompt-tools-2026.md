# Codex CLI: System Prompt and Tool Architecture Analysis

**Source:** github.com/openai/codex (Rust rewrite, `codex-rs/`)
**Date:** 2026-02-09
**Repo stats:** ~60k stars, Apache-2.0, primarily Rust (96%)

## 1. System Prompt Architecture

### Prompt Hierarchy

Codex uses a layered instruction system with **per-model prompt variants**:

| File                                      | Lines | Bytes | Used By                           |
| ----------------------------------------- | ----- | ----- | --------------------------------- |
| `prompt.md`                               | 275   | 21KB  | Base models (fallback)            |
| `prompt_with_apply_patch_instructions.md` | 351   | 24KB  | Models with apply_patch via shell |
| `gpt_5_codex_prompt.md`                   | 68    | 7KB   | GPT-5 Codex (compact version)     |
| `gpt_5_1_prompt.md`                       | 331   | -     | GPT-5.1                           |
| `gpt_5_2_prompt.md`                       | 298   | -     | GPT-5.2                           |
| `gpt-5.2-codex_instructions_template.md`  | 80    | 7KB   | GPT-5.2 Codex (templated)         |

The GPT-5 Codex prompt (68 lines) is **dramatically shorter** than the base prompt (275 lines).
The templated GPT-5.2 prompt uses `{{ personality }}` substitution.

### Prompt Assembly

System instructions are sent as the `instructions` field in OpenAI's Responses API:

```rust
pub struct ResponsesApiRequest<'a> {
    pub model: &'a str,
    pub instructions: &'a str,  // system prompt goes here
    pub input: &'a [ResponseItem],  // conversation history
    pub tools: &'a [serde_json::Value],
    pub tool_choice: &'static str,
    pub parallel_tool_calls: bool,
    pub reasoning: Option<Reasoning>,
    pub stream: bool,
    pub text: Option<TextControls>,  // verbosity control
    ...
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

### System Prompt Content (Base ~275 lines)

Major sections in the base `prompt.md`:

1. **Identity & capabilities** (3 lines) - "You are a coding agent running in the Codex CLI"
2. **Personality** (3 lines) - "concise, direct, and friendly"
3. **AGENTS.md spec** (10 lines) - Scope rules, precedence
4. **Responsiveness / Preamble messages** (20 lines) - Rules for pre-tool-call messages, with 8 examples
5. **Planning** (70 lines!) - Detailed update_plan guidance with good/bad plan examples
6. **Task execution** (20 lines) - Coding guidelines, apply_patch usage
7. **Validating work** (15 lines) - Testing philosophy
8. **Ambition vs precision** (8 lines) - Greenfield vs existing codebase
9. **Progress updates** (8 lines)
10. **Final answer formatting** (80 lines) - Very detailed formatting rules: headers, bullets, monospace, file references, tone

The GPT-5 Codex prompt (68 lines) condenses ALL of this into a compressed form:

- Editing constraints, plan tool usage, special requests, presenting work, and file reference rules
- Much more terse: "Default: be very concise; friendly coding teammate tone"

### Personality System (GPT-5.2+)

Two personalities defined as separate markdown files (~2KB each):

- **Pragmatic**: "deeply pragmatic, effective software engineer" - clarity, pragmatism, rigor
- **Friendly**: "optimize for team morale and being a supportive teammate" - warm, encouraging, uses "we"

Injected into the template via `{{ personality }}`.

### Collaboration Modes

Templates that modify behavior:

- **Default** (680 bytes): Minimal, strongly prefers executing over asking
- **Execute** (3.9KB): Independent execution, assumptions-first, long-horizon planning
- **Plan** (7KB): 3-phase planning (explore, intent chat, implementation chat), strict no-mutation rules
- **Pair Programming** (1.1KB): Build together, small steps, liberal planning tool use

## 2. Tool Definitions

### Tool Count and Types

**Core tools** (always present):

1. `shell` / `shell_command` / `exec_command` (mutually exclusive, depends on model config)
2. `write_stdin` (only with unified exec)
3. `update_plan`
4. `list_mcp_resources`
5. `list_mcp_resource_templates`
6. `read_mcp_resource`

**Conditional tools:** 7. `apply_patch` (freeform OR function variant) 8. `request_user_input` (collaboration modes only) 9. `search_tool_bm25` (MCP tool discovery) 10. `grep_files` (experimental) 11. `read_file` (experimental) 12. `list_dir` (experimental) 13. `view_image` (when model supports image input) 14. `web_search` (cached or live) 15. `spawn_agent` / `send_input` / `resume_agent` / `wait` / `close_agent` (collab/multi-agent)

Typical tool count: **6-10 core** + MCP tools. Up to ~16 with all features enabled.

### Tool Format: OpenAI Responses API

Tools are serialized as JSON objects with a `type` discriminator:

```rust
enum ToolSpec {
    Function(ResponsesApiTool),     // Standard function calling
    LocalShell {},                   // Built-in shell (no schema)
    WebSearch { external_web_access },
    Freeform(FreeformTool),         // Custom tool with grammar
}
```

#### Standard Function Tool Format

```json
{
  "type": "function",
  "name": "shell",
  "description": "Runs a shell command and returns its output.\n- The arguments to `shell` will be passed to execvp()...",
  "strict": false,
  "parameters": {
    "type": "object",
    "properties": {
      "command": {
        "type": "array",
        "items": { "type": "string" },
        "description": "The command to execute"
      },
      "workdir": {
        "type": "string",
        "description": "The working directory..."
      },
      "timeout_ms": { "type": "number", "description": "..." }
    },
    "required": ["command"],
    "additionalProperties": false
  }
}
```

#### Freeform Tool Format (GPT-5+ only)

```json
{
  "type": "custom",
  "name": "apply_patch",
  "description": "Use the `apply_patch` tool to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.",
  "format": {
    "type": "grammar",
    "syntax": "lark",
    "definition": "start: begin_patch hunk+ end_patch\nbegin_patch: \"*** Begin Patch\" LF\n..."
  }
}
```

### Tool Description Verbosity

Descriptions are **concise but functional**:

- `shell`: 2-3 lines with usage notes ("pass to execvp", "set workdir")
- `read_file`: 1 line ("Reads a local file with 1-indexed line numbers...")
- `grep_files`: 1 line
- `update_plan`: 3 lines
- `apply_patch` (JSON variant): ~60 lines with full grammar + examples (very verbose!)
- `apply_patch` (freeform): 1 line description + Lark grammar definition (20 lines)

Property descriptions are typically **one short sentence** each:

- "Shell command to execute."
- "Absolute path to the file"
- "The maximum number of lines to return."

All tools use `strict: false` (no strict JSON schema enforcement).

### Interesting Tool Design Patterns

1. **Sandbox permissions as tool params**: Shell tools include `sandbox_permissions` and `justification` fields for escalation
2. **prefix_rule param**: Lets model suggest command prefixes for future auto-approval
3. **Freeform tools**: Custom tool type with Lark grammar definition -- the model outputs raw patch text, not JSON
4. **search_tool_bm25**: Dynamic MCP tool discovery -- searches tool metadata, makes matches available for next call
5. **Agent tools**: Full sub-agent lifecycle (spawn, send, wait, resume, close) with agent role types

## 3. Message Formatting for OpenAI Models

### Responses API (not Chat Completions)

Codex uses the **Responses API**, not the Chat Completions API. Key differences:

- System prompt goes in `instructions` field (not a system message)
- Conversation items are `ResponseItem` objects (not messages)
- Native support for `local_shell` and `web_search` tool types
- `FunctionCall` and `FunctionCallOutput` are first-class item types
- `parallel_tool_calls` is a top-level parameter
- `reasoning` parameter for reasoning effort control
- `text.verbosity` for controlling output verbosity (low/medium/high)
- `prompt_cache_key` for explicit prompt caching

### Shell Output Formatting

When using freeform `apply_patch`, shell tool outputs are **re-serialized** from JSON to structured text:

```
Exit code: 0
Wall time: 1.2 seconds
Total output lines: 42
Output:
[actual output here]
```

This avoids double-JSON-encoding when the tool output format is already text-based.

### Environment Context (XML)

```xml
<environment_context>
  <cwd>/path/to/repo</cwd>
  <shell>bash</shell>
  <network enabled="true">
    <allowed>api.example.com</allowed>
  </network>
</environment_context>
```

## 4. Compact Tool Definitions

Codex does **not** use compact/minified tool definitions. Key observations:

- All property descriptions are present and human-readable
- No abbreviation of parameter names or descriptions
- `additionalProperties: false` on every object (helps model adherence)
- `strict: false` on all tools (allows flexible model output)
- Tool schemas use a subset of JSON Schema (no `oneOf`, `anyOf`, etc.)
- MCP tool schemas are **sanitized** at import time to fit this subset

The main optimization is **conditional tool inclusion**:

- Tools are added/removed based on model capabilities and feature flags
- `search_tool_bm25` hides MCP tools until searched (reduces tool count per turn)
- Experimental tools (`grep_files`, `read_file`, `list_dir`) gated behind model-level flags
- Web search can be cached or live, or omitted entirely

### Token Budget Awareness

- `auto_compact_token_limit` per model for triggering compaction
- `effective_context_window_percent: 95` (leaves headroom)
- Tool output truncation: configurable via `truncation_policy` (bytes or tokens)
- Context compaction prompt at `core/templates/compact/prompt.md`:
  "Create a handoff summary for another LLM that will resume the task"

## 5. Key Takeaways for ion

1. **Prompt size scales inversely with model capability**: GPT-5 Codex gets 68 lines vs 275 for base models. Smarter models need fewer instructions.

2. **Per-model prompt variants are standard**: Different models get different system prompts, not just different tool sets.

3. **Tool descriptions are concise but not minified**: One-sentence descriptions work well. The apply_patch grammar is the exception (needs to be verbose for correct output).

4. **Freeform/custom tools are a GPT-specific feature**: The Lark grammar approach for apply_patch avoids JSON wrapping overhead. Not portable to other providers.

5. **Responses API is purpose-built for agents**: The `instructions` + `input` + `tools` separation is cleaner than stuffing everything into messages. For non-OpenAI providers, ion would need to map this pattern onto their APIs.

6. **Collaboration modes are system prompt addons**: Plan/Execute/Pair are just extra text appended to base instructions, not separate tool sets.

7. **Multi-agent via tool calls**: Sub-agents are spawned, messaged, and waited-on through standard tool calls, not a separate protocol.

8. **Tool discovery via search**: When many MCP tools exist, `search_tool_bm25` lets the model find relevant ones without bloating every request.
