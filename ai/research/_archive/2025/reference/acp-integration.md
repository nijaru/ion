# Aircher ACP Integration Analysis

## Current Architecture vs ACP Requirements

### ✅ **Compatible Elements**

1. **Tool System** - Our `AgentTool` trait and `ToolRegistry` map well to ACP's tool calling
2. **Async Architecture** - Our async/await based system aligns with ACP's design
3. **Structured Communication** - We already have JSON-based tool parameters
4. **Session Concept** - Our `CodingConversation` provides session-like functionality
5. **Streaming Support** - Our `AgentStream` provides real-time updates

### 🔧 **Modifications Needed**

1. **Communication Protocol** - Switch from direct in-process to JSON-RPC over stdio
2. **Interface Implementation** - Implement ACP's `Agent` trait
3. **Message Format** - Standardize on ACP's message formats
4. **Bidirectional Requests** - Allow agent to request client operations
5. **Session Management** - Implement ACP's `SessionId` system

### ⚠️ **Architecture Challenges**

1. **TUI Integration** - Our tight TUI coupling needs loosening
2. **Provider Management** - ACP assumes external LLM management
3. **Direct Tool Access** - ACP expects client-mediated file operations
4. **State Management** - Need to externalize conversation state

## ACP Integration Strategy

### Phase 1: ACP Agent Implementation

**Goal**: Make Aircher work as an ACP agent that Zed (or other editors) can use

```rust
// New crate structure
[dependencies]
agent-client-protocol = "0.1.1"
tokio = { version = "1.0", features = ["full"] }
serde_json = "1.0"

// src/acp/mod.rs
pub mod agent;
pub mod client;
pub mod adapter;

// src/acp/agent.rs
use agent_client_protocol::{Agent, AgentSideConnection};

pub struct AircherAgent {
    controller: AgentController,
    tools: ToolRegistry,
    sessions: HashMap<SessionId, CodingConversation>,
}

#[async_trait]
impl Agent for AircherAgent {
    async fn initialize(&mut self, request: InitializeRequest) -> Result<InitializeResponse>;
    async fn prompt(&mut self, request: PromptRequest) -> Result<PromptResponse>;
    // ... other ACP methods
}
```

### Phase 2: Tool Adaptation

**Map our tools to ACP's client-mediated model**:

```rust
// Instead of direct file access:
impl AgentTool for ReadFileTool {
    async fn execute(&self, params: Value) -> Result<ToolOutput, ToolError> {
        // Request file read from ACP client
        let request = ClientRequest::ReadFile { path: params["path"] };
        let response = self.client_connection.request(request).await?;
        // Process response...
    }
}
```

### Phase 3: Dual Mode Operation

**Support both standalone TUI and ACP modes**:

```rust
// src/main.rs
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("--acp") => {
            // Run as ACP agent over stdin/stdout
            acp_main().await
        }
        _ => {
            // Run standalone TUI mode
            tui_main().await
        }
    }
}

async fn acp_main() -> Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let agent = AircherAgent::new().await?;
    let connection = AgentSideConnection::new(stdin, stdout, agent);

    connection.run().await
}
```

## Technical Implementation Plan

### 1. Tool Registry Adaptation

```rust
// Current: Direct tool execution
pub async fn execute_tools(&self, tool_calls: &[ToolCall]) -> Vec<(String, Result<Value, String>)>

// ACP: Client-mediated execution
pub async fn execute_tools(
    &self,
    tool_calls: &[ToolCall],
    client: &ClientConnection
) -> Vec<(String, Result<Value, String>)>
```

### 2. Session Management

```rust
pub struct AircherSessionManager {
    sessions: HashMap<SessionId, CodingConversation>,
    active_session: Option<SessionId>,
}

impl AircherSessionManager {
    pub async fn create_session(&mut self, context: ProjectContext) -> SessionId;
    pub async fn get_session(&self, id: &SessionId) -> Option<&CodingConversation>;
    pub async fn update_session(&mut self, id: &SessionId, messages: Vec<Message>);
}
```

### 3. Client Interface Implementation

```rust
#[async_trait]
impl Client for AircherClient {
    async fn read_file(&mut self, request: ReadFileRequest) -> Result<ReadFileResponse>;
    async fn write_file(&mut self, request: WriteFileRequest) -> Result<WriteFileResponse>;
    async fn list_directory(&mut self, request: ListDirectoryRequest) -> Result<ListDirectoryResponse>;
    async fn run_command(&mut self, request: RunCommandRequest) -> Result<RunCommandResponse>;
    // Permission checking, etc.
}
```

## Competitive Advantages with ACP

### 1. **Multi-Editor Support**
- Work in Zed, VS Code (future), Neovim, etc.
- Maintain terminal workflow advantages
- Expand user base beyond TUI users

### 2. **Standardized Integration**
- Follow industry standard (ACP)
- Easier adoption by other editors
- Future-proof architecture

### 3. **Flexible Deployment**
- Standalone TUI mode for terminal users
- ACP mode for editor integration
- Best of both worlds

### 4. **Enhanced Tool System**
- Permission-based tool execution
- Better security model
- Integration with editor file systems

## Migration Path

### Week 1: ACP Foundation
- [ ] Add `agent-client-protocol` dependency
- [ ] Implement basic `Agent` trait
- [ ] Create ACP message handling

### Week 2: Tool System Adaptation
- [ ] Adapt existing tools to client-mediated model
- [ ] Implement permission system
- [ ] Test tool execution via ACP

### Week 3: Session Management
- [ ] Implement ACP session handling
- [ ] Migrate conversation state management
- [ ] Test multi-session support

### Week 4: Integration Testing
- [ ] Test with Zed editor
- [ ] Validate tool execution
- [ ] Polish user experience

## Benefits Summary

1. **Standards Compliance** - Follow industry direction (ACP as "LSP for agents")
2. **Market Expansion** - Work with multiple editors, not just our TUI
3. **Future Proofing** - Align with Zed, Google, Anthropic roadmap
4. **Competitive Edge** - Model selection transparency + ACP compatibility
5. **Dual Mode** - Keep TUI advantages while adding editor integration

## Next Steps

1. **Prototype ACP Agent** - Basic implementation to validate approach
2. **Tool Adaptation** - Migrate 1-2 core tools to ACP model
3. **Zed Testing** - Validate integration with real editor
4. **Architecture Refinement** - Based on initial testing results

The architecture is **well-positioned** for ACP integration with minimal breaking changes to existing functionality.
