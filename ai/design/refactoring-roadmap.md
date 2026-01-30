# Refactoring Roadmap

Comprehensive list of refactoring work identified through code review.

## Priority Matrix

| Priority | Area                         | Effort   | Impact | Blocked By |
| -------- | ---------------------------- | -------- | ------ | ---------- |
| P0       | OAuth + Provider             | 5-7 days | High   | -          |
| P1       | App struct decomposition     | 2-3 days | High   | -          |
| P1       | main.rs render loop          | 1-2 days | Medium | -          |
| P2       | render.rs split              | 1 day    | Medium | -          |
| P2       | events.rs cleanup            | 1 day    | Medium | -          |
| P2       | Expose subagent tool         | 0.5 day  | Medium | -          |
| P3       | Magic strings cleanup        | 0.5 day  | Low    | -          |
| P3       | Chat insertion consolidation | 0.5 day  | Low    | P1         |
| P3       | Accessor method cleanup      | 0.5 day  | Low    | P1         |

---

## P0: OAuth + Provider Layer

**Design:** `ai/design/oauth-subscriptions.md`, `ai/design/provider-replacement.md`

**Tasks:** tk-uqt6, tk-7zp8, tk-3a5h, tk-toyu, tk-aq7x

These are interrelated - OAuth needs new auth infrastructure, provider replacement needs new HTTP clients. Do together.

### New Modules

```
src/
├── auth/                 # NEW
│   ├── mod.rs           # AuthMethod enum, CredentialStorage trait
│   ├── oauth.rs         # PKCE flow implementation
│   ├── server.rs        # Local callback server (localhost:port)
│   ├── storage.rs       # File/keychain credential storage
│   └── device_code.rs   # Headless auth (optional, Phase 2)
├── provider/
│   ├── mod.rs           # Add ChatGptPlus, GoogleAi variants
│   ├── http.rs          # NEW: Shared HTTP + SSE utilities
│   ├── anthropic.rs     # NEW: Native Anthropic client
│   ├── openai_compat.rs # NEW: OpenAI-compatible client
│   └── ...
```

### Provider Enum Changes

```rust
pub enum Provider {
    // API key providers (existing)
    Anthropic,
    OpenAI,
    Google,
    Ollama,
    Groq,
    OpenRouter,
    Kimi,

    // OAuth subscription providers (new)
    ChatGptPlus,   // OpenAI OAuth, uses Codex backend
    GoogleAi,      // Google OAuth, consumer Gemini
}
```

---

## P1: App Struct Decomposition

**Problem:** App has 30+ fields mixing UI state, agent state, channels, timing, and tokens.

**Current state:**

```rust
pub struct App {
    // UI state
    pub mode: Mode,
    pub selector_page: SelectorPage,
    pub input_buffer: ComposerBuffer,
    pub input_state: ComposerState,
    pub input_history: Vec<String>,
    pub history_index: usize,
    pub history_draft: Option<String>,
    pub frame_count: u64,
    pub needs_setup: bool,
    pub setup_fetch_started: bool,

    // Pickers
    pub provider_picker: ProviderPicker,
    pub model_picker: ModelPicker,
    pub session_picker: SessionPicker,

    // Render state (already extracted!)
    pub render_state: RenderState,

    // Agent/orchestration
    pub agent: Arc<Agent>,
    pub orchestrator: Arc<ToolOrchestrator>,
    pub agent_tx: mpsc::Sender<AgentEvent>,
    pub agent_rx: mpsc::Receiver<AgentEvent>,
    pub approval_rx: mpsc::Receiver<ApprovalRequest>,
    pub pending_approval: Option<ApprovalRequest>,

    // Session
    pub session: Session,
    pub store: SessionStore,
    pub session_rx: mpsc::Receiver<Session>,
    pub session_tx: mpsc::Sender<Session>,

    // Task/run state
    pub is_running: bool,
    pub task_start_time: Option<Instant>,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub current_tool: Option<String>,
    pub retry_status: Option<(String, u64)>,
    pub thinking_start: Option<Instant>,
    pub last_thinking_duration: Option<Duration>,
    pub last_task_summary: Option<TaskSummary>,

    // Config/settings
    pub tool_mode: ToolMode,
    pub api_provider: Provider,
    pub thinking_level: ThinkingLevel,
    pub token_usage: Option<(usize, usize)>,
    pub model_context_window: Option<usize>,
    pub permissions: PermissionSettings,
    pub config: Config,
    pub model_registry: Arc<ModelRegistry>,

    // Misc
    pub should_quit: bool,
    pub last_error: Option<String>,
    pub message_queue: Option<Arc<Mutex<Vec<String>>>>,
    pub cancel_pending: Option<Instant>,
    pub esc_pending: Option<Instant>,
    pub editor_requested: bool,
    pub message_list: MessageList,
}
```

**Proposed extraction:**

### TaskState (extract first - clearest boundaries)

```rust
/// State for the currently running task/agent turn.
pub struct TaskState {
    pub is_running: bool,
    pub task_start_time: Option<Instant>,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub current_tool: Option<String>,
    pub retry_status: Option<(String, u64)>,
    pub thinking_start: Option<Instant>,
    pub last_thinking_duration: Option<Duration>,
    pub last_task_summary: Option<TaskSummary>,
}

impl TaskState {
    pub fn new() -> Self { /* all None/0/false */ }

    pub fn start_task(&mut self) {
        self.is_running = true;
        self.task_start_time = Some(Instant::now());
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.current_tool = None;
        self.retry_status = None;
    }

    pub fn finish_task(&mut self, summary: TaskSummary) {
        self.is_running = false;
        self.last_task_summary = Some(summary);
        self.thinking_start = None;
    }

    pub fn elapsed(&self) -> Option<Duration> {
        self.task_start_time.map(|t| t.elapsed())
    }
}
```

### UiState (extract second)

```rust
/// UI mode and interaction state.
pub struct UiState {
    pub mode: Mode,
    pub selector_page: SelectorPage,
    pub frame_count: u64,
    pub should_quit: bool,
    pub cancel_pending: Option<Instant>,
    pub esc_pending: Option<Instant>,
    pub editor_requested: bool,
    pub needs_setup: bool,
    pub setup_fetch_started: bool,
}
```

### InputState (extract third)

```rust
/// Input buffer and history.
pub struct InputState {
    pub buffer: ComposerBuffer,
    pub state: ComposerState,
    pub history: Vec<String>,
    pub history_index: usize,
    pub history_draft: Option<String>,
}
```

### Final App Structure

```rust
pub struct App {
    // Grouped state
    pub ui: UiState,
    pub input: InputState,
    pub render: RenderState,      // Already done
    pub task: TaskState,

    // Pickers (could group but low value)
    pub provider_picker: ProviderPicker,
    pub model_picker: ModelPicker,
    pub session_picker: SessionPicker,

    // Core components (keep as-is, they're Arc'd)
    pub agent: Arc<Agent>,
    pub orchestrator: Arc<ToolOrchestrator>,
    pub model_registry: Arc<ModelRegistry>,

    // Channels (keep as-is)
    pub agent_tx: mpsc::Sender<AgentEvent>,
    pub agent_rx: mpsc::Receiver<AgentEvent>,
    pub approval_rx: mpsc::Receiver<ApprovalRequest>,
    pub session_rx: mpsc::Receiver<Session>,
    pub session_tx: mpsc::Sender<Session>,

    // Session
    pub session: Session,
    pub store: SessionStore,
    pub pending_approval: Option<ApprovalRequest>,
    pub message_list: MessageList,
    pub message_queue: Option<Arc<Mutex<Vec<String>>>>,

    // Config (could group but low value)
    pub tool_mode: ToolMode,
    pub api_provider: Provider,
    pub thinking_level: ThinkingLevel,
    pub token_usage: Option<(usize, usize)>,
    pub model_context_window: Option<usize>,
    pub permissions: PermissionSettings,
    pub config: Config,
    pub last_error: Option<String>,
}
```

**Migration strategy:**

1. Extract `TaskState` first (clearest boundaries, most reset logic)
2. Update all callers: `app.is_running` → `app.task.is_running`
3. Add helper methods as needed
4. Extract `UiState` and `InputState` similarly
5. Consider `ConfigState` if the config fields grow

---

## P1: main.rs Render Loop

**Problem:** 200+ lines of inline rendering logic with duplicated resize handling.

**Location:** `src/main.rs:238-420`

**Issues:**

1. Resize handling duplicated (event handler + frame loop check)
2. Chat insertion logic mixed with UI rendering
3. Magic escape sequences (`\x1b[3J\x1b[2J\x1b[H`)

### Proposed Changes

**1. Unify resize handling:**

```rust
impl App {
    /// Handle terminal resize - reflow chat and repaint.
    pub fn handle_resize(&mut self, w: u16, h: u16, stdout: &mut impl Write) -> io::Result<()> {
        if !self.message_list.entries.is_empty() {
            Self::clear_screen_and_scrollback();
            self.reprint_chat_scrollback(stdout, w)?;
        }
        Ok(())
    }
}

// In main loop, single call:
if size_changed {
    app.handle_resize(w, h, &mut stdout)?;
}
```

**2. Extract chat insertion:**

```rust
impl App {
    /// Insert chat lines using row-tracking or scroll mode.
    pub fn insert_chat_lines(
        &mut self,
        stdout: &mut impl Write,
        lines: Vec<StyledLine>,
        term_height: u16,
    ) -> io::Result<()> {
        // Move the 50-line chat insertion logic here
    }
}
```

**3. Named constants for escape sequences:**

```rust
// src/tui/terminal.rs
pub const CLEAR_SCREEN_AND_SCROLLBACK: &str = "\x1b[3J\x1b[2J\x1b[H";

pub fn clear_screen_and_scrollback() {
    print!("{}", CLEAR_SCREEN_AND_SCROLLBACK);
    let _ = std::io::stdout().flush();
}
```

---

## P2: render.rs Split

**Problem:** 800 lines, `render_selector_direct` alone is 250 lines.

**Location:** `src/tui/render.rs`

### Proposed Split

```
src/tui/
├── render.rs              # Core UI rendering (400 lines)
├── render_state.rs        # Already exists
├── render_selector.rs     # NEW: Selector rendering (250 lines)
└── render_progress.rs     # NEW: Progress/status (150 lines)
```

**render_selector.rs:**

- `render_selector_direct()`
- `render_provider_list()`
- `render_model_list()`
- `render_session_list()`
- Helper functions for table formatting

**render_progress.rs:**

- `render_progress_line()`
- `render_status_line()`
- `format_elapsed()`
- `format_tokens()`

---

## P2: events.rs Cleanup

**Problem:** 600 lines handling keys, slash commands, and mode transitions.

**Location:** `src/tui/events.rs`

### Proposed Split

```
src/tui/
├── events.rs           # Core event dispatch (200 lines)
├── events_keys.rs      # NEW: Key handling (200 lines)
├── events_commands.rs  # NEW: Slash commands (200 lines)
```

**events_commands.rs:**

- `handle_slash_command()`
- Individual command handlers: `/clear`, `/model`, `/provider`, `/help`, etc.
- Command parsing and validation

**events_keys.rs:**

- `handle_key_input_mode()`
- `handle_key_selector_mode()`
- `handle_key_approval_mode()`
- Platform-specific keybindings

---

## P2: Expose Subagent Tool

**Problem:** `src/agent/subagent.rs` has working subagent code but no tool exposes it.

**Location:** `src/agent/subagent.rs`, `src/tool/`

### Proposed Changes

**1. Add subagent tool:**

```rust
// src/tool/builtin/subagent.rs
pub struct SubagentTool {
    registry: Arc<SubagentRegistry>,
}

impl Tool for SubagentTool {
    fn name(&self) -> &str { "subagent" }

    fn description(&self) -> &str {
        "Delegate a task to a specialized subagent"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "Name of the subagent (e.g., 'researcher', 'reviewer')"
                },
                "task": {
                    "type": "string",
                    "description": "Task description for the subagent"
                }
            },
            "required": ["agent", "task"]
        })
    }

    async fn execute(&self, params: Value) -> Result<String> {
        let agent_name = params["agent"].as_str().unwrap();
        let task = params["task"].as_str().unwrap();

        let config = self.registry.get(agent_name)
            .ok_or_else(|| anyhow!("Unknown subagent: {}", agent_name))?;

        let result = run_subagent(config, task, self.provider.clone()).await?;
        Ok(result.output)
    }
}
```

**2. Implement tool whitelisting (TODO in subagent.rs:118):**

```rust
// Filter tools based on config.tools whitelist
let allowed_tools: HashSet<_> = config.tools.iter().collect();
let orchestrator = Arc::new(
    ToolOrchestrator::with_builtins(ToolMode::Write)
        .filter(|t| allowed_tools.is_empty() || allowed_tools.contains(&t.name()))
);
```

**3. Add default subagent configs:**

```yaml
# ~/.config/ion/subagents/researcher.yaml
name: researcher
description: Searches and analyzes information
tools: [read, glob, grep]
max_turns: 10

# ~/.config/ion/subagents/reviewer.yaml
name: reviewer
description: Reviews code for issues
tools: [read, glob, grep]
system_prompt: |
  You are a code reviewer. Analyze the code for:
  - Bugs and logic errors
  - Security vulnerabilities
  - Performance issues
  Report findings with file:line references.
max_turns: 5
```

---

## P3: Lower Priority Items

### Magic Strings Cleanup

Replace hardcoded escape sequences with named constants:

```rust
// src/tui/terminal.rs
pub mod escapes {
    pub const CLEAR_SCREEN: &str = "\x1b[2J";
    pub const CLEAR_SCROLLBACK: &str = "\x1b[3J";
    pub const CURSOR_HOME: &str = "\x1b[H";
    pub const CLEAR_ALL: &str = "\x1b[3J\x1b[2J\x1b[H";
}
```

### Chat Insertion Consolidation

Move chat insertion logic from main.rs to a method on App or RenderState (covered in P1 main.rs cleanup).

### Accessor Method Cleanup

The accessor methods in `src/tui/mod.rs` (lines 132-153) are thin wrappers:

```rust
pub fn header_inserted(&self) -> bool {
    self.render_state.header_inserted
}
```

**Options:**

1. Keep them (encapsulation, could change internal structure later)
2. Remove them (render_state is pub, adds indirection)
3. Make render_state private and keep accessors

**Recommendation:** Keep for now, revisit after App decomposition.

---

## Implementation Order

```
Phase 1: OAuth + Provider (enables free testing)
├── tk-7zp8: OAuth infrastructure
├── tk-3a5h: ChatGPT Plus/Pro
├── tk-toyu: Google AI
└── tk-aq7x: Provider replacement (can overlap)

Phase 2: App Decomposition (improves maintainability)
├── Extract TaskState
├── Extract UiState
└── Extract InputState

Phase 3: File Splits (improves navigation)
├── render.rs → render_selector.rs, render_progress.rs
├── events.rs → events_keys.rs, events_commands.rs
└── main.rs render loop cleanup

Phase 4: Features (after core is clean)
├── Expose subagent tool
├── Plugin system
└── Memory integration
```

---

## Success Metrics

After refactoring:

- No file > 500 lines (currently render.rs: 800, events.rs: 600)
- App struct < 20 direct fields (currently 30+)
- Clear module boundaries (auth/, provider/, tui/)
- Can understand any module in < 5 minutes
