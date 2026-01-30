# Module Structure

## Current Architecture

```
src/
├── agent/              # Multi-turn agent orchestration
│   ├── mod.rs          # Agent struct, run loop
│   ├── turn.rs         # Single turn handling
│   ├── subagent.rs     # Subagent spawning
│   └── types.rs        # AgentEvent, TurnResult
│
├── auth/               # OAuth + credential storage
│   ├── mod.rs          # AuthMethod enum
│   ├── oauth.rs        # PKCE flow
│   ├── server.rs       # Callback server
│   ├── storage.rs      # File-based credentials
│   └── providers/      # Per-provider configs
│
├── compaction/         # Context summarization
│   └── mod.rs          # Compaction strategies
│
├── config/             # Configuration
│   └── mod.rs          # TOML loading, defaults
│
├── mcp/                # Model Context Protocol
│   ├── mod.rs          # MCP client
│   └── types.rs        # MCP message types
│
├── provider/           # LLM API clients
│   ├── anthropic/      # Native Anthropic Messages API
│   │   ├── client.rs   # AnthropicClient
│   │   ├── request.rs  # With cache_control
│   │   ├── response.rs # With thinking blocks
│   │   └── stream.rs   # SSE event types
│   ├── openai_compat/  # OpenAI-compatible APIs
│   │   ├── client.rs   # OpenAICompatClient
│   │   ├── quirks.rs   # Provider-specific handling
│   │   ├── request.rs  # With provider routing
│   │   └── stream.rs   # Streaming types
│   ├── http/           # Shared utilities
│   │   ├── client.rs   # HttpClient wrapper
│   │   └── sse.rs      # SSE parser
│   ├── gemini_oauth.rs # Google Generative AI
│   ├── registry.rs     # Model discovery
│   ├── prefs.rs        # Provider preferences
│   └── types.rs        # Message, ContentBlock
│
├── session/            # Session persistence
│   ├── mod.rs          # Session struct
│   └── store.rs        # SQLite operations
│
├── skill/              # Skill definitions
│   ├── mod.rs          # Skill loading
│   └── types.rs        # YAML frontmatter
│
├── tool/               # Tool framework
│   ├── builtin/        # Built-in tools
│   │   ├── bash.rs
│   │   ├── edit.rs
│   │   ├── glob.rs
│   │   ├── grep.rs
│   │   ├── list.rs
│   │   ├── read.rs
│   │   └── write.rs
│   ├── orchestrator.rs # Tool routing + permissions
│   └── types.rs        # Tool trait, ToolResult
│
└── tui/                # Terminal UI
    ├── composer/       # Input buffer
    ├── render.rs       # UI rendering (needs split)
    ├── events.rs       # Input handling (needs split)
    ├── highlight.rs    # Syntax highlighting
    ├── table.rs        # Table rendering
    └── terminal.rs     # Low-level terminal ops
```

## Module Responsibilities

### provider/ - LLM Communication

**Purpose**: Unified interface to all LLM providers.

**Key types**:

- `Client` - Main entry point, routes to backend
- `LlmApi` trait - `stream()` and `complete()` methods
- `Message`, `ContentBlock` - Provider-agnostic format
- `ChatRequest` - Request with model, messages, tools

**Backends**:

- `AnthropicClient` - Native Messages API
- `OpenAICompatClient` - OpenAI/OpenRouter/Groq/Kimi/Ollama
- `GeminiOAuthClient` - Google Generative AI

### agent/ - Turn Loop

**Purpose**: Orchestrate multi-turn conversations.

**Key types**:

- `Agent` - Holds provider client, manages conversation
- `AgentEvent` - Streaming events (text, tool calls, done)
- `TurnResult` - Outcome of a single turn

### tool/ - Tool Execution

**Purpose**: Execute tools with permission control.

**Key types**:

- `Tool` trait - Name, description, parameters, execute
- `ToolOrchestrator` - Route calls, check permissions
- `ToolMode` - Read/Write/AGI permission levels

### session/ - Persistence

**Purpose**: Store conversations in SQLite.

**Key types**:

- `Session` - Conversation metadata + messages
- `SessionStore` - CRUD operations

### tui/ - User Interface

**Purpose**: Terminal rendering and input handling.

**Key patterns**:

- Chat printed to scrollback via `insert_before`
- Bottom UI cursor-positioned
- Selector overlays replace bottom UI

## Planned Improvements

### Split Large Files

| File           | Lines | Action                                        |
| -------------- | ----- | --------------------------------------------- |
| tui/render.rs  | 820   | Split: render_selector.rs, render_progress.rs |
| tui/events.rs  | 630   | Split: events_keys.rs, events_commands.rs     |
| tui/session.rs | 718   | Consider: session_picker extraction           |

### Add Context Module

For model switching and conversation portability:

```
src/context/
├── mod.rs          # Context management
├── converter.rs    # Cross-provider conversion
└── manager.rs      # Window management, truncation
```

**Responsibilities**:

- Convert messages between provider formats
- Handle tool call ID remapping
- Manage context window limits
- Trigger compaction when needed

## Tool Call ID Handling

When switching providers mid-conversation, tool call IDs need translation:

**Option A: Keep original IDs** (current)

- Simple, usually works
- May fail if provider validates ID format

**Option B: ID remapping** (recommended)

- Maintain `HashMap<original_id, new_id>` per session
- Remap on send, reverse-map on receive
- Fully portable across providers

**Option C: Strip tool calls**

- Lossy but safe
- Use only for problematic providers

Recommendation: Implement Option B in `context/converter.rs`.
