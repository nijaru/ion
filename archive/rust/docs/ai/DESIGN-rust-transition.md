# ion System Design

## Overview

ion is currently in architecture transition. The original implementation is a Rust CLI/TUI agent, and the active rewrite branch is exploring a full Go host built on Bubble Tea v2.

**Current production line**: Rust CLI/TUI (`main`, `tui-work`)
**Active rewrite line**: Go host prototype/rewrite (`codex/go-rewrite-host`)
**Decision in progress**: whether ion should become all-Go, or keep only part of the system in Rust

```
User Request → ion CLI → Agent Core → Tool Execution → Response
```

## Module Architecture

```
src/
├── agent/          # Multi-turn agent loop
├── auth/           # OAuth + credential storage
├── compaction/     # Context summarization
├── config/         # TOML config loading
├── mcp/            # Model Context Protocol client
├── provider/       # LLM API clients
│   ├── anthropic/  # Native Anthropic Messages API
│   ├── openai_compat/  # OpenAI/OpenRouter/Groq/Kimi
│   ├── http/       # Shared HTTP + SSE utilities
│   └── ...
├── session/        # SQLite persistence
├── skill/          # YAML skill definitions
├── tool/           # Built-in + MCP tools
└── tui/            # Terminal UI
```

### Provider Layer (Native HTTP)

Three protocol implementations:

| Protocol  | Providers                                       | Features                       |
| --------- | ----------------------------------------------- | ------------------------------ |
| Anthropic | Anthropic                                       | cache_control, thinking blocks |
| OpenAI    | OpenAI, ChatGPT, OpenRouter, Groq, Kimi, Ollama | provider routing, reasoning    |
| Google    | Google, Gemini                                  | function calling               |

Provider quirks handled in `openai_compat/quirks.rs`:

- `max_tokens` vs `max_completion_tokens`
- `store` field compatibility
- `developer` vs `system` role
- `reasoning_content` extraction

### Agent Layer

Current direct providers remain the primary execution path, but long-term backend structure is now split conceptually into:

- **Direct model backends**: native HTTP providers (`anthropic`, `openai_compat`, `google`, etc.)
- **Agent backends**: external agent runtimes that own approvals, tool lifecycle, and subscription auth semantics (future ACP path)

ACP work should land as an agent-backend layer above provider clients, not as another provider implementation.

### Tool Framework

- **Built-in**: `read`, `write`, `edit`, `bash`, `glob`, `grep`, `list`
- **MCP**: Client support for external tool servers
- **Permission matrix**: Read/Write modes (sandbox-based security)

## Data Model

### Message Types (provider-agnostic)

```rust
pub enum ContentBlock {
    Text { text: String },
    Thinking { thinking: String },
    ToolCall { id, name, arguments },
    ToolResult { tool_call_id, content, is_error },
    Image { media_type, data },
}

pub struct Message {
    pub role: Role,  // System, User, Assistant, ToolResult
    pub content: Arc<Vec<ContentBlock>>,
}
```

Conversion to provider format happens at request time in each client's `build_request()`.

### Persistence

```
~/.ion/
├── config.toml          # Global settings
└── data/
    └── sessions.db      # SQLite with WAL mode
```

**Tables:**

- `sessions`: metadata (id, working_dir, model, timestamps)
- `messages`: transcript (role, JSON content, position)
- `input_history`: global input recall

## Design Principles

- **Minimalist**: Focus on code and chat; avoid UI clutter
- **Native**: Leverage terminal scrollback for history
- **Simple**: Prefer clear+reprint over complex diffing
- **Provider-agnostic**: Canonical message format, convert at edges


## Rewrite Direction (2026-03-12)

The active implementation experiment is now `go-host/`, built with Bubble Tea v2 and Bubbles v2.

Current intent:

- build the host loop for real rather than maintaining a toy demo
- shape the host/backend boundary so it can later support:
  - a native ion agent runtime
  - ACP-backed external agents
  - subscription-safe hosted agent flows
- judge the rewrite by actual UX and development velocity, especially in:
  - multiline composer behavior
  - transcript/footer interaction
  - resize and redraw correctness
  - session/backend event modeling

This does not yet settle the full-system language decision, but it does mean the Go rewrite is now the active path being exercised in code.

