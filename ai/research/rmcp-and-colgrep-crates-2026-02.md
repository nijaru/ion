# Research: rmcp and colgrep Rust Crates (2026-02)

**Research Date**: 2026-02-13
**Purpose**: Evaluate rmcp as mcp-sdk-rs replacement; assess colgrep for code search

---

## 1. rmcp -- Official Rust MCP SDK

### Identity

| Field        | Value                                                                             |
| ------------ | --------------------------------------------------------------------------------- |
| Crate        | [rmcp](https://crates.io/crates/rmcp)                                             |
| Repo         | [modelcontextprotocol/rust-sdk](https://github.com/modelcontextprotocol/rust-sdk) |
| Maintainer   | MCP project (modelcontextprotocol org) -- **official**                            |
| Origin       | Created by [4t145](https://github.com/4t145/rmcp), adopted as official SDK        |
| License      | Apache-2.0                                                                        |
| Version      | **0.15.0** (Feb 10, 2026)                                                         |
| Downloads    | 3.6M total, 2.4M recent                                                           |
| Stars        | ~3,000                                                                            |
| Contributors | 136+                                                                              |
| Last commit  | Feb 10, 2026                                                                      |

### MCP Spec Compliance

Implements MCP **2025-11-25** specification. Supports the full protocol:

- Tools (with structured JSON output schemas)
- Resources
- Prompts
- Sampling
- Roots
- Logging
- Completions
- Elicitation (SEP for user input)
- Tasks (SEP-1686 for long-running async operations)

### Transport Support

| Transport                 | Feature Flag                               | Notes                      |
| ------------------------- | ------------------------------------------ | -------------------------- |
| Async Read/Write          | `transport-async-rw`                       | Foundation layer           |
| Stdio (I/O streams)       | `transport-io`                             | tokio AsyncRead/AsyncWrite |
| Child process             | `transport-child-process`                  | Spawn server as subprocess |
| Streamable HTTP client    | `transport-streamable-http-client`         | Client-agnostic            |
| Streamable HTTP (reqwest) | `transport-streamable-http-client-reqwest` | Default HTTP client        |
| Streamable HTTP server    | `transport-streamable-http-server`         | Axum-based                 |
| SSE server                | `transport-sse-server`                     | Legacy SSE support         |

### Cargo Features

- `client` -- client-side operations
- `server` -- server + tool system
- `macros` -- `#[tool]` proc macro (default)
- `auth` -- OAuth2 support
- `schemars` -- JSON Schema generation
- `reqwest` (default), `reqwest-native-tls`, `reqwest-tls-no-provider` -- TLS backends

### API Style

Uses proc macros for ergonomic server/tool definitions:

```rust
use rmcp::{ServerHandler, ServiceExt, tool, tool_router};

#[tool_router]
impl MyServer {
    #[tool(description = "Add two numbers")]
    async fn add(&self, a: i32, b: i32) -> String {
        (a + b).to_string()
    }
}
```

Client side uses typed request/response with `ServiceExt` for transport binding.

### Companion Crate

- `rmcp-macros` -- proc macros for tool generation (versioned in lockstep)

---

## 2. mcp-sdk-rs (current ion dependency)

| Field        | Value                                                         |
| ------------ | ------------------------------------------------------------- |
| Crate        | [mcp-sdk-rs](https://crates.io/crates/mcp-sdk-rs)             |
| Repo         | [jgmartin/mcp-sdk-rs](https://github.com/jgmartin/mcp-sdk-rs) |
| Maintainer   | Individual (jgmartin), forked from Derek-X-Wang               |
| License      | MIT                                                           |
| Version      | 0.3.4 (Nov 13, 2025)                                          |
| Downloads    | 6,229 total, 1,997 recent                                     |
| Stars        | 8                                                             |
| Contributors | 2                                                             |
| Last commit  | Jan 2025                                                      |
| Status       | **"Not ready for production use"** (per README)               |

### What mcp-sdk-rs Supports

- Stdio transport (via child process Session::Local)
- WebSocket transport
- No SSE or streamable HTTP
- Raw JSON-RPC request/response (manual `json!({...})` calls)
- No proc macros, no typed tool definitions
- MCP spec version: **2024-11-05** (outdated)

---

## 3. rmcp vs mcp-sdk-rs Comparison

| Dimension            | rmcp 0.15.0                                | mcp-sdk-rs 0.3.4           |
| -------------------- | ------------------------------------------ | -------------------------- |
| **Official**         | Yes (modelcontextprotocol org)             | No (individual fork)       |
| **MCP spec**         | 2025-11-25                                 | 2024-11-05                 |
| **Downloads**        | 3.6M                                       | 6.2K                       |
| **Active**           | Yes (Feb 2026)                             | Stale (Nov 2025)           |
| **Production ready** | Yes                                        | Self-described "not ready" |
| **Transports**       | stdio, child-process, streamable HTTP, SSE | stdio, WebSocket           |
| **Typed API**        | Full typed models + proc macros            | Raw JSON-RPC               |
| **Tool macros**      | `#[tool]`, `#[tool_router]`                | None                       |
| **OAuth**            | Yes                                        | No                         |
| **Resources**        | Yes                                        | No                         |
| **Prompts**          | Yes                                        | No                         |
| **Tasks**            | Yes (SEP-1686)                             | No                         |
| **Elicitation**      | Yes                                        | No                         |

### Migration Impact for ion

Ion's current MCP usage (`/Users/nick/github/nijaru/ion/src/mcp/mod.rs`) is straightforward:

- `Session::Local` for spawning child processes via stdio
- `Client::new()` for request/response
- Manual `client.request("initialize", ...)` / `client.request("tools/list", ...)` / `client.request("tools/call", ...)`
- Hardcoded protocol version `"2024-11-05"`

With rmcp, the equivalent would use:

- `TokioChildProcess` transport + `ServiceExt` for child process spawning
- Typed `ListToolsRequest` / `CallToolRequest` instead of raw JSON
- Automatic protocol negotiation instead of manual `initialize` handshake
- Feature flags: `client`, `transport-child-process`

The migration surface is small -- ~100 lines in `src/mcp/mod.rs` plus Cargo.toml.

---

## 4. colgrep -- Semantic Code Search

### Identity

| Field      | Value                                                                      |
| ---------- | -------------------------------------------------------------------------- |
| Crate      | [colgrep](https://crates.io/crates/colgrep)                                |
| Repo       | [lightonai/next-plaid](https://github.com/lightonai/next-plaid) (monorepo) |
| Maintainer | LightOn AI (raphaelsty)                                                    |
| License    | MIT                                                                        |
| Version    | 1.0.7 (Feb 13, 2026)                                                       |
| Downloads  | 290 total                                                                  |
| Edition    | 2021                                                                       |

### What It Is

**NOT a colored grep.** It is a semantic code search CLI tool powered by ColBERT (multi-vector embeddings). It parses code with tree-sitter, generates embeddings with a 17M-parameter model (LateOn-Code-edge), and uses the PLAID algorithm for compressed vector search.

### How It Works

1. **Parse** -- tree-sitter extracts functions, methods, classes from source
2. **Analyze** -- 5-layer structural analysis (AST, call graph, control flow, data flow, dependencies)
3. **Structure** -- enriched text representations of code units
4. **Encode** -- ColBERT model produces ~300 token-level 128-dim vectors per code unit
5. **Index** -- PLAID algorithm with product quantization, memory-mapped
6. **Search** -- MaxSim scoring with optional regex pre-filtering

### Features

- Semantic search (find code by meaning, not just keywords)
- Regex filtering (ERE syntax)
- Hybrid mode (semantic + regex)
- File/path filtering
- JSON output for programmatic use
- Agent integration (Claude Code, OpenCode, Codex)
- 25 languages via tree-sitter

### Dependencies (heavy)

- ONNX runtime (embedded)
- tree-sitter + 25 language grammars
- ndarray, rayon
- next-plaid, next-plaid-onnx (from same monorepo)
- Downloads ML model on first use

### Platform Support

- macOS: Apple Accelerate + CoreML acceleration
- Linux/Windows: CPU by default, optional GPU from source
- Index stored in `~/.local/share/colgrep` or `~/Library/Application Support/colgrep`

### Relevance to ion

colgrep is **not a replacement for grep-searcher/grep-regex**. It is a completely different category:

| Aspect       | grep-searcher/grep-regex      | colgrep                             |
| ------------ | ----------------------------- | ----------------------------------- |
| Type         | Exact text/regex search       | Semantic vector search              |
| Index        | None (streaming)              | Requires pre-built index            |
| Dependencies | Minimal (~2 crates)           | Heavy (ONNX, tree-sitter, ML model) |
| Latency      | Instant                       | Index build + query overhead        |
| Use case     | Built-in grep tool for agents | Codebase-wide semantic discovery    |
| Binary size  | Negligible                    | Massive (ONNX + models)             |

colgrep is a CLI tool, not a library to embed. It could hypothetically be offered as an MCP server or external tool, but not as a grep-searcher replacement inside ion.

---

## Note: thomd/colgrep (different project)

There is also [thomd/colgrep](https://github.com/thomd/colgrep), a shell script (not Rust) that colorizes pattern matches in stdin streams. 4 stars, last updated years ago. Not relevant.
