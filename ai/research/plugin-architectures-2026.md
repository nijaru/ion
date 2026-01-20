# Plugin/Extension Architectures for Coding Agents

Research on plugin and extension patterns in the AI coding agent ecosystem.

**Date**: 2026-01-12
**Status**: Complete

## Summary Table

| System      | Plugin Type     | Language            | Loading          | Security Model                  | Capabilities                            |
| ----------- | --------------- | ------------------- | ---------------- | ------------------------------- | --------------------------------------- |
| Claude Code | Skills/Plugins  | Markdown + Scripts  | Filesystem, lazy | Tool restrictions, fork context | Instructions, hooks, MCP servers        |
| Goose       | MCP Extensions  | Any (Python/TS/etc) | stdio/HTTP spawn | User approval per tool          | Tools, resources, prompts               |
| Zed         | WASM Extensions | Rust -> WebAssembly | Archive download | Wasmtime sandbox                | Languages, themes, slash commands       |
| Toad        | ACP Adapters    | Any                 | CLI spawn        | Process isolation               | Unified TUI for agent CLIs              |
| VS Code     | Extension API   | TypeScript          | Extension host   | Process isolation               | Language Model Tools, Chat Participants |

---

## 1. Claude Code: Skills and Plugins

### Architecture

Claude Code uses a **markdown-based skill system** where Claude autonomously decides which skills to invoke based on request context.

**Skill Structure**:

```
.claude/skills/my-skill/
  SKILL.md           # Instructions + YAML frontmatter
  scripts/           # Executable scripts (run without reading)
  templates/         # Reference files
```

**Plugin Structure**:

```
my-plugin/
  .claude-plugin/
    plugin.json      # Manifest (name, version, description)
  commands/          # Slash commands (markdown files)
  agents/            # Custom agent definitions
  skills/            # Bundled skills with SKILL.md files
  hooks/             # Event handlers (hooks.json)
  .mcp.json          # MCP server configurations
  .lsp.json          # LSP server configurations
```

### Loading and Discovery

1. **Discovery**: At startup, loads only `name` and `description` from YAML frontmatter
2. **Activation**: When request matches description, Claude asks user confirmation
3. **Execution**: Full SKILL.md loaded, referenced files/scripts accessed as needed
4. **Progressive Disclosure**: Three levels - metadata (always), instructions (on trigger), resources (as needed)

**Loading Sources** (precedence order):

- Enterprise managed settings
- Personal: `~/.claude/skills/`
- Project: `.claude/skills/`
- Plugins: Bundled in plugin directory

### Security Model

- **Tool Restrictions**: `allowed-tools` field limits available tools when skill is active
- **Forked Context**: `context: fork` runs skill in isolated sub-agent with separate history
- **User Consent**: Confirmation required before loading full skill content
- **Script Permissions**: Scripts need execute permissions (`chmod +x`)
- **No arbitrary code**: Skills are instructions, not executable code

**Example SKILL.md**:

```yaml
---
name: code-review
description: Reviews code for bugs, security issues, and style violations
allowed-tools: Read, Grep, Glob
context: fork
agent: Explore
---

When reviewing code:
1. Check for common security vulnerabilities
2. Verify error handling patterns
3. Run ./scripts/lint-check.sh (don't read, just execute)
```

### Capabilities

- Instructions for Claude's behavior
- Tool access restrictions
- Hook registration (PreToolUse, PostToolUse, Stop)
- Script execution without context loading
- MCP server integration
- LSP server integration for code intelligence

### Pros/Cons

**Pros**:

- Simple to create (just markdown)
- Model-invoked (no explicit calls needed)
- Progressive loading minimizes token usage
- Tool restrictions provide safety
- Scripts can encapsulate complex logic

**Cons**:

- Limited sandboxing (relies on tool approval)
- No true code execution capability in skill itself
- Discovery depends on good descriptions
- Namespace conflicts between plugins

---

## 2. Goose: MCP Extensions

### Architecture

Goose uses the **Model Context Protocol (MCP)** for all extensions. Extensions are MCP servers that communicate via JSON-RPC over stdio or HTTP.

**Extension Types**:

- **Built-in**: Developer, Computer Controller, Memory
- **External**: Any MCP server (hundreds available)
- **Custom**: User-built MCP servers

### Loading and Discovery

1. **Registration**: Extensions configured in Goose settings (Desktop UI or CLI)
2. **Transport**: stdio (local process) or HTTP/SSE (remote)
3. **Capability Advertisement**: Server declares tools, resources, prompts on connect
4. **Dynamic Discovery**: Goose suggests relevant extensions based on task context

**Configuration Example**:

```json
{
  "type": "stdio",
  "name": "my-extension",
  "command": "uv run /path/to/server.py",
  "description": "Custom Wikipedia reader"
}
```

### Security Model

- **User Approval**: Tool invocations require user consent
- **Malware Scanning**: External extensions checked before activation
- **Process Isolation**: Each MCP server runs in separate process
- **OAuth Support**: For remote MCP servers requiring authentication
- **No Sandboxing**: MCP servers have full system access (responsibility on server author)

**MCP Security Best Practices** (from specification):

- Explicit user consent for data access and operations
- Per-client consent verification
- Sandboxed execution recommended for local servers
- Access token scoping for remote servers

### Capabilities

- **Tools**: Functions the agent can call (e.g., `read_file`, `search_github`)
- **Resources**: Data sources the agent can access (e.g., database connections)
- **Prompts**: Pre-defined prompt templates
- **Sampling**: Request LLM completions from host (enables agentic tools)
- **MCP Apps**: Interactive UI components rendered in Goose Desktop

### Creating Extensions

```python
# Example MCP server using Python SDK
from mcp.server import FastMCP

app = FastMCP("wikipedia-reader")

@app.tool()
async def read_wikipedia_article(url: str) -> str:
    """Read and convert a Wikipedia article to markdown."""
    # Implementation
    return markdown_content
```

### Pros/Cons

**Pros**:

- Language-agnostic (any language with MCP SDK)
- Large ecosystem of existing MCP servers
- Standardized protocol (works with Claude, Goose, VS Code, etc.)
- Remote server support
- Sampling enables intelligent tool behavior

**Cons**:

- No sandboxing (servers have full access)
- Process spawn overhead for stdio servers
- Security relies on user vigilance
- Complex setup for remote servers

---

## 3. Zed: WASM Extensions

### Architecture

Zed extensions are written in **Rust** and compiled to **WebAssembly**, running in the **Wasmtime** runtime with sandboxed execution.

**Extension Structure**:

```
my-extension/
  Cargo.toml         # Rust project config
  extension.toml     # Extension manifest
  src/
    lib.rs           # Extension code implementing Extension trait
  languages/
    my-lang/
      config.toml
      highlights.scm  # Tree-sitter queries
```

### Loading and Discovery

1. **Registry**: Extensions listed in `zed-industries/extensions` repo (mirrors to API)
2. **Compilation**: CI compiles Rust to WASM, uploads to S3
3. **Distribution**: Archive containing `.wasm` file + metadata + queries
4. **Installation**: Downloaded, extracted to extensions directory
5. **Runtime**: Loaded into Wasmtime, async methods called by host

**Compilation Pipeline**:

```
Rust source -> cargo build --target wasm32-wasi -> extension.wasm
            -> zed-extension CLI packages with manifest
            -> CI uploads to extension registry
```

### Security Model

- **WASM Sandbox**: Extensions run in Wasmtime with restricted capabilities
- **WIT Interface**: Strictly defined API surface using WebAssembly Interface Types
- **No Direct System Access**: Extensions interact only through host-provided APIs
- **Async Boundaries**: Blocking operations in WASM are async from host perspective
- **License Requirements**: Extensions must use approved open-source licenses

**WIT (Wasm Interface Types)**:

```wit
interface extension {
    record worktree {
        id: u64,
        root-name: string,
    }

    language-server-command: func(config: worktree) -> result<command, string>;
}
```

The `wit_bindgen::generate!` macro generates Rust bindings from WIT definitions, ensuring type-safe communication between extension and host.

### Capabilities

- **Language Support**: Tree-sitter parsers, language servers, syntax highlighting
- **Themes**: Color schemes and UI themes
- **Slash Commands**: Custom commands in editor
- **Agent Servers**: AI agent integrations (planned)
- **MCP Servers**: Model Context Protocol servers (planned)
- **Debuggers**: Debugging support

### Pros/Cons

**Pros**:

- Strong sandboxing via WASM
- Type-safe interface (WIT)
- Cross-platform (WASM is portable)
- Async support without extension awareness
- Safe to run untrusted code

**Cons**:

- Rust-only (other languages can target WASM but less ergonomic)
- Compilation step required
- Limited API surface currently
- Registry is centralized

---

## 4. ACP and Toad

### Two "ACP" Protocols

There are two distinct protocols abbreviated "ACP":

**Agent Communication Protocol** (IBM/BeeAI):

- Agent-to-agent communication
- REST-based, HTTP-native
- Now merged into **A2A (Agent2Agent)** under Linux Foundation
- Focus: Multi-agent orchestration and interoperability

**Agent Client Protocol** (agentclientprotocol.com):

- Editor/IDE to coding agent communication
- Similar to LSP (Language Server Protocol) but for agents
- JSON-RPC over stdio (local) or HTTP/WebSocket (remote)
- Focus: Standardizing agent UX across tools

### Toad and Agent Client Protocol

**Toad** (by Will McGugan, creator of Rich/Textual) is a terminal UI that provides a unified interface for multiple AI coding agents using the **Agent Client Protocol**.

**Supported Agents** (as of Dec 2025):

- Claude Code
- OpenHands
- Gemini CLI
- Aider
- And 8+ more

**Architecture**:

```
Toad (Terminal UI)
    |
    +-- ACP JSON-RPC over stdio
    |       |
    |       +-- Claude Code process
    |       +-- OpenHands process
    |       +-- Gemini CLI process
    |
    +-- Unified UI features
            - Fuzzy file search
            - @ mentions for context
            - Session management
```

### Agent Client Protocol Details

**Purpose**: Standardize communication between code editors/IDEs and coding agents (like LSP for language servers).

**Transport**:

- Local: JSON-RPC over stdio
- Remote: HTTP or WebSocket (work in progress)

**Message Format**:

- Markdown for user-readable text
- JSON for structured data
- Custom types for agentic UX (diffs, file changes)

**Key Operations**:

- Initialize session
- Send user message
- Receive agent response (streaming)
- Handle tool calls and confirmations
- Display diffs and file changes

### IBM Agent Communication Protocol (A2A)

**Purpose**: Agent-to-agent communication for multi-agent systems.

**Architecture**:

- RESTful API
- Supports sync, async, and streaming
- Framework-agnostic (works with LangChain, CrewAI, BeeAI, etc.)

**Key Concepts**:

- `AgentManifest`: Describes agent capabilities for discovery
- `Message`: Communication unit (multimodal)
- `Run`: Execution instance (sync, async, or interactive)

**Current Status**: Merged into A2A under Linux Foundation governance.

### Pros/Cons

**Toad/Agent Client Protocol**:

**Pros**:

- Unified UX across different agent CLIs
- Process isolation (each agent is separate process)
- Simple integration (just implement stdio protocol)
- Preserves each agent's strengths

**Cons**:

- Limited to what agents expose via ACP
- Adds abstraction layer
- Young protocol (still evolving)
- Not a true plugin system (adapter pattern)

---

## 5. Other Plugin Patterns

### VS Code Language Model Tools

VS Code provides multiple extensibility patterns for AI:

**Language Model Tools**:

- Registered in `package.json` under `languageModelTools`
- Invoked automatically by Agent mode based on prompt
- Run in extension host process (full VS Code API access)
- JSON schema defines input parameters

**MCP Tools**:

- Integrated via JSON config or programmatically
- Run as separate processes
- Standardized protocol

**Chat Participants**:

- Custom chat personas (e.g., `@workspace`, `@terminal`)
- Respond to specific @ mentions
- Can invoke tools and access context

### GitHub Copilot Extensions

Two patterns:

**Skillsets** (lightweight):

- Declarative API definitions
- Automatic routing and prompt crafting
- Minimal code required

**Agents** (full control):

- Custom request/response handling
- Integration with external LLMs
- Function calling support

### OpenHands SDK

Two-layer composability:

**Deployment Level**:

- SDK, Tools, Workspace, Agent Server
- Flexible deployment (local, hosted, containerized)

**Capability Level**:

- Typed component model
- Extend tools, LLMs, contexts
- Safe extension through interfaces

---

## Design Recommendations for Aircher

Based on this research, consider these patterns:

### Recommended: Hybrid Approach

1. **Skills (Claude Code pattern)** for instructions/knowledge
   - Simple markdown format
   - Model-invoked based on description
   - Progressive loading for token efficiency
   - Tool restrictions for safety

2. **MCP Integration** for tool extensions
   - Leverage existing MCP ecosystem
   - Stdio transport for local tools
   - Standardized tool/resource/prompt interface

3. **Consider WASM** for untrusted extensions (future)
   - Stronger sandboxing guarantees
   - More complex but safer
   - Good for third-party extensions

### Key Design Principles

| Principle             | Implementation                          |
| --------------------- | --------------------------------------- |
| Progressive Loading   | Metadata first, content on demand       |
| User Consent          | Confirm before loading external content |
| Tool Restrictions     | Allow skills to limit available tools   |
| Process Isolation     | Run extensions in separate processes    |
| Standardized Protocol | Use MCP for tool interoperability       |

### Security Hierarchy

1. **Instructions only** (Skills): Low risk, just prompt modifications
2. **MCP tools with approval**: Medium risk, user confirms each action
3. **WASM sandboxed**: Medium-high capability, strong isolation
4. **Native extensions**: High capability, requires trust

---

## References

- [Claude Code Skills Documentation](https://code.claude.com/docs/en/skills)
- [Claude Code Plugins Documentation](https://code.claude.com/docs/en/plugins)
- [Goose Architecture](https://block.github.io/goose/docs/goose-architecture/)
- [Goose Custom Extensions Tutorial](https://block.github.io/goose/docs/tutorials/custom-extensions/)
- [Zed Extensions Blog: Life of a Zed Extension](https://zed.dev/blog/zed-decoded-extensions)
- [Zed Extensions Part 1](https://zed.dev/blog/language-extensions-part-1)
- [MCP Specification](https://modelcontextprotocol.io/specification/2025-11-25)
- [MCP Security Best Practices](https://modelcontextprotocol.io/specification/draft/basic/security_best_practices)
- [Agent Client Protocol](https://agentclientprotocol.com/)
- [Toad by Will McGugan](https://willmcgugan.github.io/toad-released/)
- [IBM Agent Communication Protocol](https://research.ibm.com/projects/agent-communication-protocol)
- [A2A Protocol](https://a2a-protocol.org/latest/)
- [VS Code AI Extensibility](https://code.visualstudio.com/api/extension-guides/ai/ai-extensibility-overview)
