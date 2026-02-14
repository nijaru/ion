# Tool Architecture Survey: TUI Coding Agents (February 2026)

**Research Date**: 2026-02-14
**Purpose**: Compare tool architectures across major coding agents -- built-in tools, extended/on-demand tools, MCP integration, provider-agnosticism
**Prior research**: claude-code-system-prompt-2026.md, codex-cli-system-prompt-tools-2026.md, pi-mono-architecture-2026.md, extensibility-systems-2026.md, feature-gap-analysis-2026-02.md

---

## Summary

Every agent converges on the same 5-7 built-in tools (read, write, edit, bash, glob, grep). Differentiation happens in three areas: (1) how extended tools are discovered and loaded, (2) how MCP tools are managed to avoid context bloat, and (3) whether tools are provider-agnostic or tied to a specific LLM vendor.

| Agent       | Built-in    | Extended/On-Demand               | MCP             | Provider-Agnostic         |
| ----------- | ----------- | -------------------------------- | --------------- | ------------------------- |
| Claude Code | 18+         | ToolSearch (lazy MCP), Skills    | Client + Server | No (Anthropic only)       |
| Gemini CLI  | 10          | Discovery command, Extensions    | Client + Server | No (Google only)          |
| OpenCode    | 10+         | Custom tools (TS/JS), Plugins    | Client          | Yes (75+ providers)       |
| Pi-mono     | 7           | Extensions (TS runtime)          | No (by design)  | Yes (20+ providers)       |
| Codex CLI   | 6-16        | search_tool_bm25 (MCP discovery) | Client + Server | No (OpenAI only)          |
| Amp         | ~6 + skills | Toolboxes (executables), Skills  | Client          | No (multi-model, managed) |

---

## 1. Claude Code (Anthropic)

### Built-in Tools (Always Available)

| Tool            | Purpose                                                                          |
| --------------- | -------------------------------------------------------------------------------- |
| Read            | Files, images, PDFs, notebooks. Absolute paths, cat -n format, 2000 line default |
| Write           | Overwrite files. Must read first. Prefers editing existing                       |
| Edit            | Exact string replacement. old_string must be unique. replace_all for bulk        |
| Bash            | Shell execution, timeout, persistent working dir, shell state does not persist   |
| Glob            | Fast pattern matching (ripgrep-based), sorted by mtime                           |
| Grep            | Ripgrep-based. Regex, glob/type filters. content/files_with_matches/count modes  |
| Task            | Launch sub-agents (explore, plan, general). Background execution                 |
| TodoWrite       | Task tracking for 3+ step work. States: pending/in_progress/completed            |
| EnterPlanMode   | Switch to read-only planning mode                                                |
| ExitPlanMode    | Exit planning mode                                                               |
| Skill           | Execute skills (slash commands like /commit, /review-pr)                         |
| WebSearch       | Real-time web search with domain filtering                                       |
| WebFetch        | Fetch URL content, process with AI model, 15-min cache                           |
| AskUserQuestion | Ask user for clarification                                                       |
| SendMessageTool | Send messages (team coordination)                                                |
| Computer        | Browser automation (Chrome)                                                      |
| LSP             | Language Server Protocol integration                                             |
| NotebookEdit    | Jupyter notebook cell editing                                                    |
| Sleep           | Pause execution                                                                  |
| ToolSearch      | Find additional MCP tools on demand (see below)                                  |
| TeammateTool    | Coordinate with team agents                                                      |

### Extended/On-Demand Tools: ToolSearch

**Architecture**: When MCP tool descriptions exceed ~10% of context window (~10K tokens), Claude Code switches from eager loading to lazy loading.

**How it works**:

1. Detection: checks if MCP tool descriptions exceed threshold
2. Deferral: tools marked with `defer_loading: true`
3. Search tool injection: Claude receives ToolSearch instead of all definitions
4. On-demand discovery: Claude searches using regex or BM25 keywords
5. Selective loading: 3-5 relevant tools (~3K tokens) loaded per query

**Impact**: 85% token reduction (134K -> 5K in internal testing). Accuracy improvement: Opus 4 MCP eval 49% -> 74%; Opus 4.5 79.5% -> 88.1%.

### MCP Integration

- **Client**: Consumes external MCP servers via stdio, HTTP, SSE
- **Server**: Exposes tools (Bash, Read, Write, Edit, Grep, Glob) to other clients
- **Config**: `.mcp.json` per project or user
- **Tool naming**: `mcp__<server>__<tool>` prefix convention

### Skills System

Progressive disclosure:

1. Startup: load only name + description from YAML frontmatter (~50 tokens)
2. On match: Claude requests confirmation to load
3. Activation: full SKILL.md content loaded

Skills can restrict which tools the agent can use via `allowed-tools` frontmatter.

### Hooks (Lifecycle Events)

12 events: SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, PostToolUseFailure, Notification, SubagentStart, SubagentStop, Stop, PreCompact, SessionEnd

3 hook types: command (shell), prompt (single-turn LLM), agent (multi-turn subagent)

### Provider-Agnostic?

No. Anthropic models only. Tool definitions use Anthropic's native format.

---

## 2. Gemini CLI (Google)

### Built-in Tools (Always Available)

| Tool          | Internal Name       | Purpose                                                   |
| ------------- | ------------------- | --------------------------------------------------------- |
| ReadFile      | `read_file`         | Read single file content (absolute path required)         |
| WriteFile     | `write_file`        | Write content to a file                                   |
| EditTool      | `edit`              | In-place file modifications                               |
| ShellTool     | `run_shell_command` | Execute shell commands (requires confirmation for writes) |
| GrepTool      | `grep`              | Search patterns in files                                  |
| GlobTool      | `glob`              | Find files matching glob patterns                         |
| LSTool        | `ls`                | List directory contents                                   |
| ReadManyFiles | `read_many_files`   | Read/concatenate multiple files or glob patterns          |
| WebFetchTool  | `web_fetch`         | Fetch content from URLs                                   |
| WebSearchTool | `google_web_search` | Google Search grounding (built-in)                        |
| MemoryTool    | `save_memory`       | Save/recall information across sessions                   |

### Extended/On-Demand Tools

**Discovery command**: Advanced users can define `tools.discoveryCommand` in `settings.json`. The command outputs a JSON array of `FunctionDeclaration` objects. A corresponding `tools.callCommand` handles execution. These become `DiscoveredTool` instances.

**Extensions**: Gemini CLI's primary extensibility mechanism (launched October 2025). Extensions bundle MCP servers + context + commands:

```
my-extension/
  gemini-extension.json    # Manifest
  GEMINI.md                # Context/instructions for the AI
  mcp-servers/             # Bundled MCP server code
  commands/                # Custom slash commands
```

70+ extensions at launch (Dynatrace, Elastic, Figma, Harness, Postman, Shopify, Snyk, Stripe, community).

### MCP Integration

- **Client**: Supports stdio, SSE, Streamable HTTP transports
- **Server**: Can act as MCP server
- **Config**: `mcpServers` in `settings.json`
- **Tool naming**: `serverAlias__actualToolName` prefix
- **Sandboxing**: Tools subject to sandbox restrictions (Docker or `sandbox-exec`)

### Tool Registration Architecture

`ToolRegistry` class in `packages/core/src/tools/`:

- Registers all built-in tools
- Exposes `FunctionDeclaration` schemas to Gemini model
- Retrieves tools by name for execution
- `BaseTool` interface: `name`, `displayName`, `validateToolParams()`, `execute()`

### Provider-Agnostic?

No. Google Gemini models only. Tools use Google's `FunctionDeclaration` format.

---

## 3. OpenCode (anomalyco)

### Built-in Tools (Always Available)

| Tool      | Purpose                                                                     |
| --------- | --------------------------------------------------------------------------- |
| bash      | Execute shell commands                                                      |
| read      | Read file contents                                                          |
| write     | Create or overwrite files                                                   |
| edit      | Surgical text replacement                                                   |
| patch     | Apply unified diffs                                                         |
| grep      | Regex search (ripgrep under the hood)                                       |
| glob      | File pattern matching (ripgrep)                                             |
| list      | Directory listing (ripgrep)                                                 |
| ask       | Request user input/clarification                                            |
| webfetch  | Fetch and read web pages                                                    |
| websearch | Web search (Exa-based, requires env var)                                    |
| lsp       | LSP operations (experimental, requires OPENCODE_EXPERIMENTAL_LSP_TOOL=true) |
| task      | Sub-agent delegation                                                        |

### Extended/On-Demand Tools: Custom Tools

TypeScript/JavaScript tool definitions in `.opencode/tools/` (project) or `~/.config/opencode/tools/` (global):

```typescript
import { tool } from "@opencode-ai/plugin";
export default tool({
  description: "Query the project database",
  args: { query: tool.schema.string().describe("SQL query to execute") },
  async execute(args) {
    /* implementation */
  },
});
```

- Filename becomes tool name
- Multiple exports per file create multiple tools (`filename_exportName`)
- Tool definitions can invoke scripts in any language (Python, Go, etc.)
- Tools receive context: `{ agent, sessionID, messageID, directory, worktree }`
- Uses Zod for argument validation

### Built-in Agents

| Agent   | Purpose              | Restrictions        |
| ------- | -------------------- | ------------------- |
| build   | Default, full access | None                |
| plan    | Read-only analysis   | Denies edits        |
| general | Complex searches     | Invoke via @general |

### MCP Integration

- **Client**: Supports MCP servers in `opencode.json`
- **Tool naming**: `servername_*` prefix, controllable via permission wildcards
- **No server mode**: Not documented

### Permission System

Per-tool permissions in `opencode.json`:

```json
{
  "permission": {
    "edit": "deny",
    "bash": "ask",
    "webfetch": "allow",
    "mymcp_*": "ask"
  }
}
```

### Plugin System

`@opencode-ai/plugin` SDK. Plugins can provide custom tools, authentication providers, hooks, and more. Distributed via npm packages.

### LSP Integration

Auto-detects and configures 40+ LSP servers per language. Operations: goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, goToImplementation, prepareCallHierarchy, incomingCalls, outgoingCalls.

### Provider-Agnostic?

Yes. 75+ providers via models.dev. Anthropic, OpenAI, Google, local models (Ollama, etc.).

---

## 4. Pi-mono (badlogic/Mario Zechner)

### Built-in Tools (Always Available)

| Tool  | Purpose                                                  |
| ----- | -------------------------------------------------------- |
| read  | File reading                                             |
| write | File creation                                            |
| edit  | Surgical text replacement (exact match + fuzzy fallback) |
| bash  | Shell execution                                          |
| grep  | Content search                                           |
| find  | File discovery                                           |
| ls    | Directory listing                                        |

**Philosophy**: Only 4 core tools needed (read, write, edit, bash). grep/find/ls are convenience wrappers. System prompt is <1,000 tokens. "Frontier models have been RL-trained up the wazoo, so they inherently understand what a coding agent is."

### Extended Tools: Extension System

TypeScript extensions loaded at runtime via jiti. Located in `~/.pi/agent/extensions/` or `.pi/extensions/`:

```typescript
export default function(pi: ExtensionAPI) {
  pi.registerTool({ ... });        // Custom LLM-callable tools
  pi.registerCommand("name", {});  // Slash commands
  pi.registerShortcut("ctrl+k", {});
  pi.registerProvider("name", {}); // Custom LLM providers
  pi.on("tool_call", handler);     // Intercept/block tool calls
  pi.on("tool_result", handler);   // Modify tool results
}
```

91 example extensions (from permission gates to DOOM overlay to custom providers).

### Edit Tool Design

- Exact match first, fuzzy fallback (Unicode normalization, smart quotes, trailing whitespace)
- Uniqueness validation (rejects multiple matches)
- BOM stripping, line ending normalization
- Pluggable operations interface (for SSH/cloud editing)
- Abort signal handling at multiple checkpoints

### MCP Integration

**None by design**. Rationale: popular MCP servers (Playwright: 21 tools/13.7K tokens, Chrome DevTools: 26 tools/18K tokens) dump tool descriptions into context every session. Alternative: CLI tools with README files. Agent reads docs on-demand, paying token cost only when needed.

### Skills System

Follows agentskills.io spec. Three sources with precedence:

1. User global: `~/.pi/agent/skills/`
2. Project local: `.pi/skills/`
3. Explicit paths

### Package Distribution

Distribute customizations via npm or git with `pi` field in package.json:

```json
{
  "pi": {
    "extensions": ["./extensions"],
    "skills": ["./skills"],
    "prompts": ["./prompts"],
    "themes": ["./themes"]
  }
}
```

### Provider-Agnostic?

Yes. 20+ providers. Unified `stream()` and `complete()` API. API type on Model determines which provider handles it (multiple vendors share one implementation).

---

## 5. Codex CLI (OpenAI)

### Built-in Tools (Conditional)

**Core tools (always present)**:

| Tool                                 | Purpose                                                             |
| ------------------------------------ | ------------------------------------------------------------------- |
| shell / shell_command / exec_command | Shell execution (mutually exclusive variants based on model config) |
| write_stdin                          | Stdin input for unified exec                                        |
| update_plan                          | Planning tool for tracking progress                                 |
| list_mcp_resources                   | List MCP resources                                                  |
| list_mcp_resource_templates          | List MCP resource templates                                         |
| read_mcp_resource                    | Read an MCP resource                                                |

**Conditional tools (feature/model-gated)**:

| Tool                                                         | Condition                                |
| ------------------------------------------------------------ | ---------------------------------------- |
| apply_patch                                                  | Freeform (GPT-5+) or function variant    |
| request_user_input                                           | Collaboration modes only                 |
| search_tool_bm25                                             | MCP tool discovery (when many MCP tools) |
| grep_files                                                   | Experimental flag                        |
| read_file                                                    | Experimental flag                        |
| list_dir                                                     | Experimental flag                        |
| view_image                                                   | When model supports image input          |
| web_search                                                   | Cached or live mode                      |
| spawn_agent / send_input / resume_agent / wait / close_agent | Multi-agent collaboration                |

Typical count: 6-10 core + MCP tools. Up to ~16 with all features.

### Freeform Tools (GPT-5+ Only)

`apply_patch` uses a custom tool type with Lark grammar definition instead of JSON Schema. The model outputs raw patch text, not JSON-wrapped content. This avoids double-encoding overhead. Not portable to other providers.

### Extended Tools: search_tool_bm25

When many MCP tools are configured, Codex uses `search_tool_bm25` for dynamic MCP tool discovery:

- Searches tool metadata using BM25 similarity
- Makes matched tools available for the next API call
- Reduces per-turn tool count

### MCP Integration

- **Client**: stdio and streamable HTTP transports
- **Server**: `codex mcp-server` mode
- **Config**: `~/.codex/config.toml`
- **CLI management**: `codex mcp add/remove/list/login`
- **Tool naming**: `mcp__<server>__<tool>` prefix
- **Sanitization**: MCP tool schemas sanitized at import to fit JSON Schema subset (no `oneOf`, `anyOf`)

### Per-Model Tool Variants

Different models get different tool sets and system prompts:

| Model       | Prompt Size | Tools Style                         |
| ----------- | ----------- | ----------------------------------- |
| Base models | 275 lines   | JSON function calling               |
| GPT-5 Codex | 68 lines    | Freeform apply_patch + Lark grammar |
| GPT-5.1     | 331 lines   | Mixed                               |
| GPT-5.2     | 298 lines   | Templated personality               |

### Collaboration Modes

Modes modify tool availability:

- **Default**: Minimal, prefers executing over asking
- **Execute**: Independent, long-horizon planning
- **Plan**: 3-phase planning, strict no-mutation rules
- **Pair Programming**: Small steps, liberal planning tool use

### Provider-Agnostic?

No. OpenAI Responses API only. Uses native `instructions` + `input` + `tools` separation, `FunctionCall`/`FunctionCallOutput` item types, `parallel_tool_calls`, `reasoning` params.

---

## 6. Amp (Sourcegraph -> Amp Inc.)

### Built-in Tools

Amp does not publicly enumerate all built-in tools, but the architecture includes:

- File read/write/edit
- Shell execution (bash)
- Code search
- `amp tools list` shows all available tools

**Permission defaults**: Common dev commands (`ls`, `git status`, `npm test`, `cargo build`) run without prompting. Destructive commands (`git push`, `rm -rf`) require confirmation.

### Extended Tools: Toolboxes

Simple executables in directories specified by `AMP_TOOLBOX` env var:

```bash
export AMP_TOOLBOX="$PWD/.amp/tools:$HOME/.config/amp/tools"
```

Each executable responds to `TOOLBOX_ACTION` env var:

- `describe`: output JSON with name, description, args schema to stdout
- `execute`: receive args on stdin, execute, write output to stdout

Any language works. Sits between MCP servers (complex) and CLI tools (no schema).

CLI helpers: `amp tools make <name>`, `amp tools show <name>`, `amp tools use <name>`.

### Extended Tools: Skills

Skills replaced custom commands (January 2026). Packages of instructions + resources:

- `.agents/skills/` (project) or `~/.config/agents/skills/` or `~/.config/amp/skills/`
- YAML frontmatter with glob patterns for scoped application
- Can bundle `mcp.json` -- MCP servers start but tools remain hidden until skill is loaded
- Precedence: user > amp > project > claude compat > plugins > toolbox directories

### Specialized Sub-agents

| Agent        | Model              | Purpose                         |
| ------------ | ------------------ | ------------------------------- |
| Smart (main) | Claude Opus 4.6    | Default coding                  |
| Rush         | Claude Haiku 4.5   | Fast, cheap tasks               |
| Deep         | GPT-5.2 Codex      | Extended thinking               |
| Search       | Gemini 3 Flash     | Codebase retrieval              |
| Oracle       | GPT-5.2            | Complex reasoning/review        |
| Librarian    | Claude Sonnet 4.5  | External code research (GitHub) |
| Review       | Gemini 3 Pro       | Bug identification, code review |
| Look At      | Gemini 3 Flash     | Image/PDF/media analysis        |
| Painter      | Gemini 3 Pro Image | Image generation/editing        |

### MCP Integration

- **Client**: Yes, configured via settings
- **Skill-bundled MCP**: Skills can include `mcp.json`. Servers start but tools hidden until skill loaded (solves context bloat)
- **No server mode**: Not documented

### Provider-Agnostic?

Partially. Amp uses multiple models from multiple providers (Anthropic, OpenAI, Google) but users do not choose -- Amp manages model selection per task. No BYOK/isolated mode (removed May 2025).

---

## Cross-Agent Comparison

### Tool Count

| Agent       | Core Built-in | Max with Extensions          | MCP Overhead Strategy                         |
| ----------- | ------------- | ---------------------------- | --------------------------------------------- |
| Claude Code | 18+           | Unlimited (MCP)              | ToolSearch lazy loading (10K token threshold) |
| Gemini CLI  | 10            | Unlimited (Extensions + MCP) | Sandboxing, discovery command                 |
| OpenCode    | 10+           | Unlimited (Plugins + MCP)    | Permission-based gating                       |
| Pi-mono     | 7             | Unlimited (Extensions)       | No MCP (by design)                            |
| Codex CLI   | 6-16          | Unlimited (MCP)              | search_tool_bm25 BM25 discovery               |
| Amp         | ~6 + skills   | Unlimited (Toolboxes + MCP)  | Skill-bundled MCP (hidden until loaded)       |

### Core Tool Overlap

| Capability    | Claude Code     | Gemini CLI        | OpenCode   | Pi-mono | Codex CLI          | Amp          |
| ------------- | --------------- | ----------------- | ---------- | ------- | ------------------ | ------------ |
| Read file     | Read            | read_file         | read       | read    | read_file\*        | Yes          |
| Write file    | Write           | write_file        | write      | write   | apply_patch        | Yes          |
| Edit file     | Edit            | edit              | edit       | edit    | apply_patch        | Yes          |
| Shell         | Bash            | run_shell_command | bash       | bash    | shell              | Yes          |
| Glob          | Glob            | glob              | glob       | -       | list_dir\*         | Yes          |
| Grep          | Grep            | grep              | grep       | grep    | grep_files\*       | Yes          |
| Dir listing   | -               | ls                | list       | ls      | list_dir\*         | Yes          |
| Multi-read    | -               | read_many_files   | -          | -       | -                  | -            |
| Web search    | WebSearch       | google_web_search | websearch  | -       | web_search         | -            |
| Web fetch     | WebFetch        | web_fetch         | webfetch   | -       | -                  | -            |
| Sub-agents    | Task            | -                 | task       | -       | spawn_agent        | Oracle, etc. |
| Task tracking | TodoWrite       | -                 | -          | -       | update_plan        | -            |
| LSP           | LSP             | -                 | lsp (exp.) | -       | -                  | -            |
| Memory        | -               | save_memory       | -          | -       | -                  | -            |
| Image view    | Read (images)   | -                 | -          | -       | view_image         | Look At      |
| User input    | AskUserQuestion | -                 | ask        | -       | request_user_input | -            |

\* = experimental/conditional

### MCP Context Bloat Solutions

| Agent       | Strategy               | Mechanism                                                       |
| ----------- | ---------------------- | --------------------------------------------------------------- |
| Claude Code | Lazy loading           | ToolSearch tool with regex/BM25; defer_loading when >10K tokens |
| Codex CLI   | Search-based discovery | search_tool_bm25 hides tools until searched                     |
| Amp         | Skill-bundled hiding   | MCP servers start but tools hidden until skill activates        |
| Pi-mono     | No MCP                 | CLI tools + README files; agent reads docs on-demand            |
| OpenCode    | Permission gating      | Per-tool allow/deny/ask; no lazy loading documented             |
| Gemini CLI  | Extension packaging    | Extensions bundle MCP + context; no documented lazy loading     |

### Tool Definition Format

| Agent       | Provider        | Format                                                             |
| ----------- | --------------- | ------------------------------------------------------------------ |
| Claude Code | Anthropic       | Anthropic tool_use format (name, description, input_schema)        |
| Gemini CLI  | Google          | FunctionDeclaration (Google Generative AI format)                  |
| OpenCode    | Multi           | Provider-agnostic (transforms per provider)                        |
| Pi-mono     | Multi           | TypeBox schemas, converted per provider                            |
| Codex CLI   | OpenAI          | Responses API (function, local_shell, web_search, custom/freeform) |
| Amp         | Multi (managed) | Not public                                                         |

### External Tool Integration Mechanisms

| Mechanism            | Claude Code        | Gemini CLI             | OpenCode     | Pi-mono          | Codex CLI | Amp       |
| -------------------- | ------------------ | ---------------------- | ------------ | ---------------- | --------- | --------- |
| MCP (stdio)          | Yes                | Yes                    | Yes          | No               | Yes       | Yes       |
| MCP (HTTP/SSE)       | Yes                | Yes                    | Yes          | No               | Yes       | Yes       |
| MCP server mode      | Yes                | Yes                    | No           | No               | Yes       | No        |
| Custom tools (code)  | Skills             | Extensions             | Plugins (TS) | Extensions (TS)  | Skills    | Toolboxes |
| Discovery command    | -                  | tools.discoveryCommand | -            | -                | -         | -         |
| Package distribution | Plugins (git)      | Extensions (git)       | npm packages | npm/git packages | -         | -         |
| Hooks/lifecycle      | 12 events, 3 types | No                     | Plugin hooks | Extension events | No        | No        |

---

## Key Patterns

### 1. The Convergent Minimum

All agents agree on: read, write, edit, bash. Search tools (glob, grep) are present in all but some make them bash wrappers. Web tools are increasingly built-in rather than MCP-only.

### 2. Lazy Loading is the Solved Pattern for MCP Bloat

Claude Code's ToolSearch (10K threshold -> search index) and Codex's search_tool_bm25 are the two proven approaches. Amp's skill-bundled hiding is a third. Pi-mono sidesteps the problem entirely by rejecting MCP.

### 3. Provider-Agnostic Agents Need Cross-Provider Tool Format

OpenCode and Pi-mono support many providers, requiring tool definition translation. Both use an intermediate schema (Zod/TypeBox) that converts to each provider's native format. Single-provider agents (Claude Code, Codex, Gemini CLI) use native formats directly.

### 4. Tool Descriptions are Concise, Not Minified

All agents use human-readable one-sentence descriptions. No minification. apply_patch grammar (Codex) is the exception -- verbose because correctness requires it.

### 5. Extended Tools Follow Two Models

- **Code-based** (Pi-mono extensions, OpenCode plugins, Amp toolboxes): Write code that registers tools
- **Document-based** (Claude Code skills, Codex skills, Amp skills): Markdown files with metadata that teach the agent methodology; the agent uses existing tools differently, not new tools

### 6. Sub-agent Tool Sets

| Agent       | Sub-agent Approach                                   | Tool Restrictions                                  |
| ----------- | ---------------------------------------------------- | -------------------------------------------------- |
| Claude Code | Task tool spawns explore/plan/general agents         | Explore: read-only. Plan: read-only. General: full |
| Codex CLI   | spawn_agent/send_input/resume_agent/wait/close_agent | Max 6 sub-agents, role-based                       |
| Amp         | Specialized models per task                          | Each agent has fixed model + capability            |
| OpenCode    | @general, plan agents                                | plan: denies edits                                 |
| Pi-mono     | None (spawn pi via bash for full observability)      | N/A                                                |

---

## Relevance to Ion

### Current Ion Tools

read, write, edit, bash, glob, grep (6 tools). MCP client with lazy loading. Skills system.

### Validated by Survey

1. **Core 6 tools are the right set.** All agents converge on read/write/edit/bash + search. Ion has this.
2. **MCP lazy loading matters.** Ion already has lazy MCP loading -- this is correct.
3. **Skills for progressive disclosure.** Ion has the infrastructure (590 LOC). Ship default skills.
4. **Provider-agnostic tool format.** Ion needs tool definition translation per provider (already does this).

### Gaps to Consider

| Gap                               | Priority     | Rationale                                                             |
| --------------------------------- | ------------ | --------------------------------------------------------------------- |
| Web tools (search/fetch) built-in | Already done | Ion has native web_search + web_fetch                                 |
| Sub-agent tool (Task)             | Low-Medium   | Useful for context isolation, but Pi-mono shows it is unnecessary     |
| ToolSearch-style discovery        | Low          | Only matters with many MCP servers; ion already has lazy loading      |
| Hooks wiring                      | Medium       | Framework exists, needs config integration (see feature-gap-analysis) |
| LSP tool                          | Low          | Evidence inconclusive (Nuanced eval: 720 runs, negative/mixed)        |
| Memory tool                       | Low          | No agent has shipped this successfully                                |

### Anti-Patterns to Avoid

1. **Eager MCP loading** -- always lazy-load when tool definitions exceed threshold
2. **Too many built-in tools** -- Claude Code's 18+ tools means ~18+ tool definitions in every request. Token cost adds up. Keep it minimal.
3. **Provider-specific tool formats leaked into core** -- keep tool definitions in a neutral intermediate format, convert at the provider boundary
4. **Freeform/custom tool types** -- Codex's Lark grammar for apply_patch is OpenAI-specific. Do not invest in provider-specific tool innovations.

---

## References

**Claude Code**:

- [Piebald-AI/claude-code-system-prompts](https://github.com/Piebald-AI/claude-code-system-prompts) -- 133 prompt segments extracted
- [ToolSearch announcement](https://venturebeat.com/orchestration/claude-code-just-got-updated-with-one-of-the-most-requested-user-features) -- VentureBeat, Jan 15 2026
- [MCP Tool Search explained](https://www.techbuddies.io/2026/01/18/how-claude-codes-new-mcp-tool-search-slashes-context-bloat-and-supercharges-ai-agents/) -- TechBuddies
- [Advanced Tool Use guide](https://www.digitalapplied.com/blog/claude-advanced-tool-use-mcp-guide) -- defer_loading, BM25, programmatic calling

**Gemini CLI**:

- [Architecture overview](https://geminicli.com/docs/architecture/) -- Core/CLI split
- [Tools API docs](https://gemini-cli.xyz/docs/en/core/tools-api) -- ToolRegistry, BaseTool, discovery
- [Built-in tools](https://gemini-cli.xyz/docs/en/tools) -- Complete tool list
- [Extensions guide](https://geminicli.com/extensions/) -- MCP + context bundles
- [GitHub repo](https://github.com/google-gemini/gemini-cli) -- 93.7K stars, Apache-2.0

**OpenCode**:

- [Built-in tools](https://opencode.ai/docs/tools/) -- Tool list + permissions
- [Custom tools](https://opencode.ai/docs/custom-tools/) -- TS/JS tool definitions
- [SDK docs](https://opencode.ai/docs/sdk/) -- @opencode-ai/sdk
- [DeepWiki analysis](<https://deepwiki.com/anomalyco/opencode/6-node.js-implementation-(codex-cli)>) -- Tool system internals

**Pi-mono**:

- [GitHub repo](https://github.com/badlogic/pi-mono) -- packages/coding-agent/src/core/tools/
- [Blog post](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/) -- Philosophy, minimal tools
- [Extension API](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent/src/core/extensions/) -- TS runtime extensions

**Codex CLI**:

- [GitHub repo](https://github.com/openai/codex) -- 60K+ stars, Rust
- [CLI features](https://developers.openai.com/codex/cli/features/) -- Web search, skills, MCP
- [DeepWiki tool system](<https://deepwiki.com/openai/codex/6-node.js-implementation-(codex-cli)>) -- Tool registry, MCP integration

**Amp**:

- [Owner's Manual](https://ampcode.com/manual) -- Tools, skills, toolboxes, modes
- [Toolboxes announcement](https://ampcode.com/news/toolboxes) -- AMP_TOOLBOX, executable tools
- [More Tools for the Agent](https://ampcode.com/news/more-tools-for-the-agent) -- amp tools make/show/use
- [Models page](https://ampcode.com/models) -- Specialized sub-agents per task

**Cross-agent**:

- [2026 CLI tools comparison (Tembo)](https://www.tembo.io/blog/coding-cli-tools-comparison) -- 15 agents compared
- [Extensibility systems (ion research)](file:///Users/nick/github/nijaru/ion/ai/research/extensibility-systems-2026.md)
- [Feature gap analysis (ion research)](file:///Users/nick/github/nijaru/ion/ai/research/feature-gap-analysis-2026-02.md)
