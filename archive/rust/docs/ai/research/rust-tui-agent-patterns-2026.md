# Rust TUI Agent Patterns 2025-2026

**Research Date**: 2026-01-12
**Purpose**: Validate architecture for Rust TUI coding agent
**Focus**: Codex CLI, Goose, ratatui + tokio patterns

---

## Executive Summary

Your architecture design is well-aligned with current best practices. Key validations:

| Choice              | Status         | Notes                                   |
| ------------------- | -------------- | --------------------------------------- |
| ratatui + crossterm | Validated      | Industry standard, mature async support |
| tokio runtime       | Validated      | Required for concurrent TUI + async ops |
| mpsc channels       | Validated      | Canonical pattern for TUI streaming     |
| Tool orchestrator   | Validated      | Matches Codex CLI exactly               |
| Skills system       | Validated      | Claude Code pattern adopted widely      |
| MCP client          | Validated      | Table stakes for modern agents          |
| Native memory       | Differentiator | Goose uses MCP; native is advantage     |

**Key Finding**: Goose (25k+ stars) and Codex CLI prove Rust TUI agents work at scale. Your architecture closely matches their patterns.

---

## 1. Codex CLI Architecture Patterns

### Multi-Turn Task Loop

The core pattern from Codex CLI that drives 57.8% Terminal-Bench accuracy:

```rust
// Codex pattern: continue until model stops
pub async fn run_task(&mut self, task: &str) -> Result<()> {
    self.session.add_user_message(task);

    loop {
        let response = self.provider.stream(self.history.clone()).await?;

        // KEY: Empty response = task complete (not fixed iterations)
        if response.is_empty() {
            break;
        }

        for item in response.items {
            match item {
                ResponseItem::ToolCall(call) => {
                    let result = self.orchestrator.execute(call).await?;
                    self.session.add_tool_result(result);
                }
                ResponseItem::Text(text) => {
                    self.event_tx.send(AgentEvent::TextDelta(text)).await?;
                }
            }
        }

        // Proactive compaction before hitting limit
        if self.should_compact() {
            self.compact().await?;
        }
    }
    Ok(())
}
```

### Tool Orchestrator Pattern

Three-stage dispatch with approval caching:

```rust
pub struct ToolOrchestrator {
    registry: ToolRegistry,
    approval_cache: RwLock<HashSet<String>>,  // Session-scoped
    sandbox: Option<SandboxConfig>,
}

impl ToolOrchestrator {
    pub async fn execute(&self, call: &ToolCall, ctx: &ToolContext) -> Result<ToolResult> {
        // Stage 1: Approval check (cached for session)
        if !self.is_approved(&call.name).await {
            let approved = self.request_approval(&call).await?;
            if approved {
                self.cache_approval(&call.name).await;
            } else {
                return Err(OrchestratorError::Denied);
            }
        }

        // Stage 2: Execute (with sandbox if configured)
        let result = match self.execute_with_sandbox(&call, ctx).await {
            Ok(r) => r,
            Err(ToolError::SandboxDenied(reason)) => {
                // Stage 3: Escalation on sandbox failure
                if self.request_escalation(&call, &reason).await? {
                    self.execute_unboxed(&call, ctx).await?
                } else {
                    return Err(OrchestratorError::Denied);
                }
            }
            Err(e) => return Err(e.into()),
        };

        Ok(result)
    }
}
```

### Sub-Agent Pattern (Review Task)

Codex uses dedicated sub-agents for code review:

```rust
pub struct ReviewTask {
    // Separate session, stripped tools, review-specific prompt
}

impl SessionTask for ReviewTask {
    async fn run(&mut self) -> Option<String> {
        // Run single turn with REVIEW_PROMPT
        // Disabled: web search, images
        // Output: structured JSON with P0-P3 priority findings
        let response = self.sub_codex.run_single_turn().await?;
        self.parse_review_output(response)
    }
}
```

---

## 2. Goose Architecture Patterns

### Extension System (MCP-First)

Goose treats ALL tools as MCP extensions:

```
crates/
├── goose           # Core: agents, providers, context_mgmt, session
├── goose-cli       # CLI entry point
├── goose-server    # Backend binary (goosed)
├── goose-mcp       # MCP extensions
├── mcp-client      # Became official Rust MCP SDK
├── mcp-core        # Shared MCP types
└── mcp-server      # MCP server impl
```

**Key Insight**: Goose's mcp-client became the official Rust MCP SDK. Consider using it.

### Interactive Loop

```
Human Request
    │
Provider Chat (with tool list)
    │
Model Extension Call (JSON tool request)
    │
Response to Model (tool results)
    │
Context Revision (summarize, delete old)
    │
Model Response → Loop or Complete
```

### Context Revision

Goose manages tokens via:

1. **Summarization** with smaller/faster LLMs
2. **Algorithmic deletion** of old/irrelevant content
3. **Find/replace** instead of full file rewrites
4. **ripgrep** to skip system files

---

## 3. ratatui + tokio Patterns

### Canonical Async TUI Architecture

```rust
use crossterm::event::EventStream;
use futures::StreamExt;
use tokio::sync::mpsc;

pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stderr>>,
    event_tx: mpsc::UnboundedSender<Event>,
    event_rx: mpsc::UnboundedReceiver<Event>,
    task: JoinHandle<()>,
    cancellation_token: CancellationToken,
}

impl Tui {
    pub fn start(&mut self) {
        let tick_delay = Duration::from_secs_f64(1.0 / self.tick_rate);
        let render_delay = Duration::from_secs_f64(1.0 / self.frame_rate);
        let event_tx = self.event_tx.clone();
        let cancel = self.cancellation_token.clone();

        self.task = tokio::spawn(async move {
            let mut reader = EventStream::new();
            let mut tick_interval = tokio::time::interval(tick_delay);
            let mut render_interval = tokio::time::interval(render_delay);

            loop {
                let tick = tick_interval.tick();
                let render = render_interval.tick();
                let crossterm_event = reader.next().fuse();

                tokio::select! {
                    _ = cancel.cancelled() => break,

                    maybe_event = crossterm_event => {
                        if let Some(Ok(evt)) = maybe_event {
                            match evt {
                                CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => {
                                    event_tx.send(Event::Key(key)).unwrap();
                                }
                                CrosstermEvent::Resize(x, y) => {
                                    event_tx.send(Event::Resize(x, y)).unwrap();
                                }
                                _ => {}
                            }
                        }
                    }

                    _ = tick => event_tx.send(Event::Tick).unwrap(),
                    _ = render => event_tx.send(Event::Render).unwrap(),
                }
            }
        });
    }

    pub async fn next(&mut self) -> Option<Event> {
        self.event_rx.recv().await
    }
}
```

### Streaming Content Updates

For LLM streaming responses in TUI:

```rust
pub enum AgentEvent {
    StreamStart,
    TextDelta(String),
    ThinkingDelta(String),
    ToolCallStart { id: String, name: String, args: Value },
    ToolCallEnd { id: String, result: ToolResult },
    TurnComplete,
    Error(String),
}

// Agent spawns streaming task, sends events via channel
pub async fn run(&mut self) -> Result<()> {
    let (stream_tx, mut stream_rx) = mpsc::channel(32);

    // Spawn streaming task
    let provider = self.provider.clone();
    let request = self.build_request();
    tokio::spawn(async move {
        provider.stream(request, stream_tx).await
    });

    // Process stream events (non-blocking)
    while let Some(event) = stream_rx.recv().await {
        self.event_tx.send(event.into()).await?;
    }

    Ok(())
}
```

### Main Loop Pattern

```rust
impl App {
    pub async fn run(&mut self, terminal: &mut Terminal<impl Backend>) -> Result<()> {
        loop {
            // Draw UI
            terminal.draw(|f| self.draw(f))?;

            // Handle events with timeout for streaming
            tokio::select! {
                // Terminal events (keyboard, resize)
                Some(event) = self.tui.next() => {
                    self.handle_terminal_event(event)?;
                }

                // Agent events (streaming, tool results)
                Some(event) = self.agent_rx.recv() => {
                    self.handle_agent_event(event)?;
                }

                // Tick for streaming refresh
                _ = tokio::time::sleep(Duration::from_millis(16)) => {}
            }

            if self.should_quit {
                break;
            }
        }
        Ok(())
    }
}
```

---

## 4. Anti-Patterns to Avoid

### Blocking in Async Context

**CRITICAL**: Never block inside tokio tasks.

```rust
// BAD: Blocks executor thread
async fn bad_example() {
    std::thread::sleep(Duration::from_secs(1));  // BLOCKS!
    std::fs::read_to_string("file.txt");         // BLOCKS!
}

// GOOD: Use async equivalents or spawn_blocking
async fn good_example() {
    tokio::time::sleep(Duration::from_secs(1)).await;
    tokio::fs::read_to_string("file.txt").await?;

    // For CPU-intensive work
    tokio::task::spawn_blocking(|| {
        expensive_computation()
    }).await?;
}
```

### Drop Implementations That Block

Some types have blocking drop implementations (sockets, files). Use `Drop` carefully in async context.

```rust
// Potential issue: TcpListener::drop() may block
struct MyHandler {
    listener: std::net::TcpListener,  // Blocking drop!
}

// Solution: Use tokio equivalents
struct MyHandler {
    listener: tokio::net::TcpListener,  // Non-blocking
}
```

### Self Borrowing in Spawned Tasks

```rust
// BAD: self escapes method body
pub async fn bad_spawn(&mut self) {
    tokio::spawn(async move {
        self.do_something();  // ERROR: self borrowed
    });
}

// GOOD: Clone what you need
pub async fn good_spawn(&mut self) {
    let tx = self.action_tx.clone();
    tokio::spawn(async move {
        tx.send(Action::Something).unwrap();
    });
}
```

### Fixed Iteration Limits

```rust
// BAD: Arbitrary iteration limit
for _ in 0..10 {
    let response = model.complete().await?;
    // May stop prematurely or continue unnecessarily
}

// GOOD: Natural completion detection (Codex pattern)
loop {
    let response = model.complete().await?;
    if response.is_empty() {
        break;  // Model decided it's done
    }
    process(response).await?;
}
```

---

## 5. Key Recommendations

### Architecture Validated

Your design in `/Users/nick/github/nijaru/aircher/ai/design/rust-architecture.md` closely matches proven patterns:

| Your Design       | Codex CLI         | Goose               | Status  |
| ----------------- | ----------------- | ------------------- | ------- |
| Provider trait    | Yes               | Yes (20+ providers) | Correct |
| Tool orchestrator | Yes (exact match) | MCP-based           | Correct |
| Skills system     | Yes               | Yes (SKILL.md)      | Correct |
| MCP client        | Yes               | Yes (wrote the SDK) | Correct |
| Multi-turn loop   | Yes               | Yes                 | Correct |

### High-Value Patterns to Implement

1. **Approval Caching** (Codex)
   - Ask once per session, cache decision
   - Escalate on sandbox failure
   - 84% reduction in permission prompts (Claude Code stat)

2. **Auto-Compaction** (Codex/Goose)
   - Monitor token usage in real-time
   - Compact at ~80-90% of context window
   - Don't interrupt task, inline compaction

3. **Sub-Agent Review** (Codex)
   - Separate agent for verification
   - Stripped tool set, focused prompt
   - Structured output (P0-P3 priorities)

4. **Context Revision** (Goose)
   - Use smaller LLM for summarization
   - Delete old/irrelevant messages algorithmically
   - Prefer find/replace over full file rewrites

### Dependencies to Use

```toml
[dependencies]
# Core
tokio = { version = "1", features = ["full"] }
ratatui = "0.29"
crossterm = { version = "0.28", features = ["event-stream"] }

# Async utilities
futures = "0.3"
async-trait = "0.1"
tokio-util = { version = "0.7", features = ["sync"] }

# HTTP + Streaming
reqwest = { version = "0.12", features = ["json", "stream"] }
reqwest-eventsource = "0.6"

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"

# Error handling
thiserror = "2"
anyhow = "1"
color-eyre = "0.6"

# CLI
clap = { version = "4", features = ["derive"] }
directories = "5"

# Memory (your differentiator)
omendb = "0.0.23"
rusqlite = { version = "0.32", features = ["bundled"] }
tiktoken-rs = "0.6"

# Utilities
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1", features = ["v4"] }
```

### MCP Client Option

Consider using Goose's mcp-client crate (now official Rust MCP SDK):

```toml
mcp-client = { git = "https://github.com/block/goose", package = "mcp-client" }
```

---

## 6. Differentiators vs Competition

| Feature          | Goose               | Codex CLI   | Claude Code | Aircher                      |
| ---------------- | ------------------- | ----------- | ----------- | ---------------------------- |
| Memory           | Tag-based MCP       | None        | None        | **Native OmenDB**            |
| Memory Retrieval | Full context inject | N/A         | N/A         | **Budget-aware, RRF hybrid** |
| Distribution     | Desktop + CLI       | CLI         | CLI         | **Single binary**            |
| Provider         | 20+                 | OpenAI-only | Claude-only | OpenRouter + direct          |
| Local-first      | Sort of             | No          | No          | **Yes**                      |

**Your key differentiator**: Native memory with budget-aware context assembly. Goose injects all memories; you can do selective, relevance-based retrieval.

---

## 7. Implementation Priorities

### Phase 1: Foundation (Validated)

- Provider trait + OpenRouter
- Basic TUI with streaming
- Agent loop (no tools)

### Phase 2: Tools (Critical Path)

- Tool trait + registry
- Tool orchestrator with approval caching
- Builtin tools (read, write, edit, glob, grep, bash)

### Phase 3: Memory (Differentiator)

- Native OmenDB integration
- ACE scoring + time decay
- RRF hybrid retrieval
- Budget-aware assembly

### Phase 4: Polish

- Skills system
- MCP client (3rd party tools)
- Context compaction
- Sub-agent patterns

---

## References

**Primary Sources**:

- Goose: https://github.com/block/goose (25k+ stars)
- Codex CLI: https://github.com/openai/codex
- ratatui async template: https://github.com/ratatui-org/async-template

**Documentation**:

- Goose Architecture: https://block.github.io/goose/docs/goose-architecture/
- ratatui Async Tutorial: https://ratatui.rs/tutorials/counter-async-app/
- Tokio Channels: https://tokio.rs/tokio/tutorial/channels

**Anti-Patterns**:

- Common Async Mistakes: https://www.elias.sh/posts/common_mistakes_with_async_rust
- Qovery Async Pitfalls: https://www.qovery.com/blog/common-mistakes-with-rust-async
