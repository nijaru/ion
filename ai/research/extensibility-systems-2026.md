# Extensibility Systems in Coding Agents (February 2026)

**Research Date**: 2026-02-06
**Purpose**: How major coding agents handle extensibility, and what ion should adopt
**Status**: Complete

---

## Executive Summary

Every major coding agent converged on the same layered extensibility model by early 2026:

| Layer                  | Purpose                            | Who Has It                                                   |
| ---------------------- | ---------------------------------- | ------------------------------------------------------------ |
| **Rules/Instructions** | Constraints, project context       | All agents (AGENTS.md, CLAUDE.md, GEMINI.md, .windsurfrules) |
| **Skills**             | Reusable methodology + knowledge   | Claude Code, Codex CLI, Amp, Crush                           |
| **Hooks**              | Deterministic lifecycle actions    | Claude Code, Cursor, Amp                                     |
| **Extensions (TS)**    | Runtime-loaded TypeScript modules  | Pi-Mono (jiti)                                               |
| **MCP**                | Authenticated external tool access | All except Pi-Mono                                           |
| **Plugins**            | Bundled packages of the above      | Claude Code, Gemini CLI                                      |
| **Subagents**          | Isolated agent contexts            | Claude Code, Codex, Amp, OpenCode, Cursor                    |

**Key finding**: MCP alone is not sufficient. The industry consensus is that Skills (knowledge/guidance) and MCP (tool access) are complementary layers. Skills are the recipe; MCP is the kitchen. Hooks provide deterministic guarantees outside the LLM reasoning loop.

**Recommendation for ion**: Start with Skills (SKILL.md) + MCP client + Hooks. These three provide 90%+ of the value with manageable complexity. Plugins (bundled distribution) can come later. WASM and embedded JS/TS are unnecessary at this stage.

---

## 1. Claude Code

**The most complete extensibility system as of February 2026.**

### Extension Points (5 layers)

| Component          | Format                      | Purpose                                        |
| ------------------ | --------------------------- | ---------------------------------------------- |
| **Slash Commands** | `.claude/commands/*.md`     | User-triggered actions                         |
| **Subagents**      | `.claude/agents/*.md`       | Isolated agent contexts with tool restrictions |
| **Skills**         | `.claude/skills/*/SKILL.md` | Model-invoked knowledge + scripts              |
| **Hooks**          | JSON in settings files      | Deterministic lifecycle automation             |
| **MCP Servers**    | `.mcp.json`                 | External tool connections                      |

### Plugin System (October 2025)

Bundles all of the above into a distributable package:

```
my-plugin/
  .claude-plugin/
    plugin.json          # Manifest: name, version, description
  commands/              # Slash commands (.md)
  agents/                # Subagent definitions (.md)
  skills/                # SKILL.md per subdirectory
  hooks/
    hooks.json           # Lifecycle hooks
  .mcp.json              # MCP server definitions
  scripts/               # Helper utilities
```

Installation: `/plugin marketplace add user-or-org/repo-name`
Distribution: Git repos with `.claude-plugin/marketplace.json`

### Hooks Architecture (comprehensive)

12 lifecycle events, 3 hook types:

**Events**: SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, PostToolUseFailure, Notification, SubagentStart, SubagentStop, Stop, PreCompact, SessionEnd

**Hook Types**:

- `command` -- Shell command (deterministic)
- `prompt` -- Single-turn LLM evaluation (yes/no judgment)
- `agent` -- Multi-turn subagent with tool access (verification)

**Communication**: stdin (JSON event data) -> script -> stdout/stderr + exit code

- Exit 0: proceed (stdout added to context for some events)
- Exit 2: block action (stderr becomes Claude's feedback)
- Structured JSON output for finer control (allow/deny/ask)

**Matchers**: Regex patterns to filter when hooks fire (e.g., `Edit|Write` for file tools, `mcp__github__.*` for GitHub MCP tools)

**Storage scopes**: User (`~/.claude/settings.json`), Project (`.claude/settings.json`), Local (`.claude/settings.local.json`), Enterprise (managed policy), Plugin-bundled, Skill/agent frontmatter

### Skills Loading (progressive disclosure)

1. Startup: Load only `name` + `description` from YAML frontmatter (~50 tokens)
2. On match: Claude requests user confirmation to load
3. Activation: Full SKILL.md loaded, scripts/templates accessible
4. Tool restrictions: `allowed-tools` field limits what the agent can do

### What Makes It Work

The power is in **composition**: a plugin can bundle a skill that teaches Claude how to use an MCP server, with hooks that enforce quality gates, and commands for manual triggers. Each layer addresses a different concern.

---

## 2. Codex CLI (OpenAI)

**Config-driven extensibility, no plugin packaging system.**

### Extension Points

| Component     | Format                    | Purpose                   |
| ------------- | ------------------------- | ------------------------- |
| **AGENTS.md** | Markdown                  | Project context and rules |
| **Skills**    | Markdown (details sparse) | Reusable knowledge        |
| **MCP**       | `~/.codex/config.toml`    | External tools            |
| **Rules**     | Config file               | Execution policies        |

### MCP Integration

```bash
codex mcp add <name> -- <command...>          # stdio server
codex mcp add <name> --url https://...        # HTTP server
codex mcp add <name> --url ... --bearer-token # authenticated
codex mcp login <name>                        # OAuth flow
codex mcp list                                # list configured
```

Can also run itself as an MCP server: `codex mcp-server`

### What's Missing

- No hooks/lifecycle events
- No plugin packaging or distribution
- No slash commands
- Skills system exists but is minimally documented
- Extension model is purely declarative (config files + MCP)

### Assessment

Codex CLI prioritizes the cloud Codex app and IDE extension over CLI extensibility. The CLI is functional but not designed as an extensibility platform.

---

## 3. Gemini CLI (Google)

**Extension system launched October 2025, MCP-based.**

### Extension Architecture

Extensions bundle MCP servers + context into installable packages:

```
my-extension/
  gemini-extension.json    # Manifest
  GEMINI.md                # Context/instructions for the AI
  mcp-servers/             # Bundled MCP server code
  commands/                # Custom slash commands
```

### Installation

```bash
gemini extensions install https://github.com/username/extension-name
gemini extensions install ./local-extension-folder
gemini extensions new my-extension mcp-server    # scaffold
```

### Key Design Choice

Extensions are essentially MCP servers with an intelligence layer (GEMINI.md context). Google's framing: "MCP servers provide raw tool connectivity, while extensions add intelligence, context, and best practices."

70+ extensions available at launch from Dynatrace, Elastic, Figma, Harness, Postman, Shopify, Snyk, Stripe, and community.

### What's Missing

- No hooks/lifecycle events
- No skill system beyond GEMINI.md
- No subagent definitions in extensions
- Limited to MCP as the tool mechanism

---

## 4. Amp (Sourcegraph)

**Skills-first architecture, replaced custom commands with skills in January 2026.**

### Extension Points

| Component       | Format                                    | Purpose                       |
| --------------- | ----------------------------------------- | ----------------------------- |
| **Skills**      | `.agents/skills/*/` with YAML frontmatter | Knowledge + bundled resources |
| **MCP Servers** | `amp mcp add` or skill-bundled `mcp.json` | External tools                |
| **Toolboxes**   | Shell executables in `AMP_TOOLBOX` dirs   | Simple deterministic tools    |
| **Checks**      | `.agents/checks/`                         | Review criteria               |
| **AGENTS.md**   | Markdown                                  | Project context               |
| **Subagents**   | Via Task tool                             | Parallel isolated contexts    |

### Skill-Bundled MCP (notable pattern)

Skills can include `mcp.json` to bundle MCP servers. The servers start when Amp launches but their tools remain hidden until the skill is loaded. This solves the context bloat problem: tools only appear in context when relevant.

### Toolboxes (unique to Amp)

Simple shell script extensions without MCP overhead. Executables respond to `TOOLBOX_ACTION` environment variables, accept stdin parameters. For deterministic tasks (database queries, test execution) that do not need the MCP protocol layer.

### Recent Changes (January 2026)

- Removed custom commands in favor of skills ("Slashing Custom Commands")
- Removed TODO list feature
- Removed Fork command
- Added subagent management panel
- Philosophy: simplify, do not accumulate features

---

## 5. Cursor / Windsurf (IDE Agents)

### Cursor

**MCP as primary extension mechanism, plus VS Code extension ecosystem.**

- MCP servers: configured in settings, managed via `/mcp` commands
- One-click MCP setup with OAuth support
- Hooks: run scripts at key agent loop points (format after edits, gate commands)
- Subagents: specialized for codebase research, terminal commands, parallel work
- Background Agents: autonomous parallel coding (0.50 release)
- Built on VS Code, inherits entire extension ecosystem
- `.cursorrules` for project context

### Windsurf

**VS Code fork with Cascade agent, Open VSX extensions.**

- Cascade agent: Write Mode, Chat Mode, Turbo Mode
- `.windsurfrules` for project constraints
- MCP server support
- Open VSX Registry for editor extensions
- Wave 13: parallel multi-agent sessions, Git worktrees
- Workflows: reusable markdown commands
- No custom tool/skill system beyond rules and MCP

### IDE Agent Pattern

Both inherit VS Code's extension model (Language Model Tools, Chat Participants) and add MCP on top. Their "extensibility" is largely the VS Code ecosystem plus MCP. They do not have skill systems or lifecycle hooks comparable to CLI agents.

---

## 6. Pi-Mono (badlogic)

**TypeScript extensions loaded at runtime via jiti. No MCP by design.**

### Extension System

Extensions are TS modules in `~/.pi/agent/extensions/` or `.pi/extensions/`. Each exports a factory receiving `ExtensionAPI`:

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

### Why No MCP

Pi deliberately rejects MCP for context bloat reasons:

> "Playwright MCP (21 tools, 13.7k tokens) or Chrome DevTools MCP (26 tools, 18k tokens) dump their entire tool descriptions into your context."

Alternative: CLI tools with README files. Agent reads docs when needed (progressive disclosure).

### Key Patterns for ion

- **Tool interception** via events — extensions can block dangerous commands or modify results
- **Pluggable operations** — all tools accept operation interfaces for remote execution (SSH, containers)
- **Edit tool fuzzy matching** — exact match first, then Unicode-normalized fallback (smart quotes, trailing whitespace). Handles real LLM failure modes.
- **91 example extensions** — from permission gates to DOOM overlay to custom providers

---

## 7. MCP: Sufficient or Not?

### What MCP Provides

- Standardized tool interface (JSON-RPC)
- Transport flexibility (stdio, HTTP, SSE)
- Language-agnostic (any language can implement)
- Authentication/OAuth flows
- Resource access (databases, APIs, files)
- Prompt templates
- Sampling (request LLM completions from host)
- Massive ecosystem (thousands of servers)

### What MCP Cannot Do

| Gap                           | Why MCP Falls Short                                          | What Fills It                                 |
| ----------------------------- | ------------------------------------------------------------ | --------------------------------------------- |
| **Context bloat**             | 50+ tools consume ~72K tokens before work starts             | Skills (lazy loading, progressive disclosure) |
| **Usage guidance**            | MCP describes tools but not when/how to use them effectively | Skills (methodology, best practices)          |
| **Lifecycle automation**      | No hook into agent events (pre-edit, post-edit, stop)        | Hooks                                         |
| **Deterministic enforcement** | MCP tools are model-invoked, not guaranteed                  | Hooks (always run)                            |
| **Project rules**             | MCP has no concept of project-specific constraints           | Rules files (AGENTS.md, etc.)                 |
| **Distribution**              | No packaging/installation standard                           | Plugins                                       |

### Industry Consensus (February 2026)

"Skills are the recipe. MCP is the kitchen." -- Multiple sources

MCP is necessary but not sufficient. The full system is:

1. **Rules** -- constraints (always loaded)
2. **Skills** -- knowledge + methodology (loaded on demand)
3. **MCP** -- tool access (always available but tool descriptions lazy-loaded)
4. **Hooks** -- deterministic lifecycle guarantees (always active)

---

## 8. TypeScript/WASM Extension Mechanisms for Rust CLIs

### Option A: Embedded JavaScript/TypeScript (rustyscript / deno_core)

**rustyscript** wraps `deno_core` to provide JS/TS execution in Rust:

| Aspect          | Details                                                  |
| --------------- | -------------------------------------------------------- |
| **Crate**       | `rustyscript` (wraps `deno_core` which wraps `rusty_v8`) |
| **TypeScript**  | Supported, transpiled to JS before execution             |
| **Sandboxing**  | Default: no filesystem or network access                 |
| **Async**       | Full async JS support, configurable threading            |
| **Binary size** | Significant: V8 adds ~30-50MB to binary                  |
| **Build time**  | V8 compilation is slow (minutes)                         |
| **Memory**      | Each V8 isolate: ~2-10MB baseline                        |

**Pros**: Full JS/TS compatibility, strong sandboxing, Deno ecosystem
**Cons**: Massive binary size increase, slow builds, heavy dependency chain

### Option B: WASM Plugins (Extism / Wasmtime)

**Extism** provides a higher-level plugin framework over Wasmtime:

| Aspect            | Details                                          |
| ----------------- | ------------------------------------------------ |
| **Host SDK**      | Rust crate `extism`                              |
| **Guest PDKs**    | Rust, Go, C, Haskell, AssemblyScript, Zig        |
| **Communication** | Function calls with serialized data (JSON bytes) |
| **Sandboxing**    | WASM sandbox, no host access by default          |
| **WASI**          | Preview 2 supported, sandboxed filesystem access |
| **Binary size**   | Wasmtime adds ~5-15MB                            |
| **Build time**    | Moderate                                         |

**Direct Wasmtime + WIT** gives maximum control but more boilerplate.

**Pros**: Strong sandboxing, cross-language, lighter than V8, portable
**Cons**: Worse developer experience (compile step), limited ecosystem for agent plugins, no TypeScript without compilation to WASM (which is non-trivial)

### Option C: Process-Based Extensions (MCP pattern)

**Spawn external processes communicating via stdio/HTTP.**

| Aspect          | Details                                        |
| --------------- | ---------------------------------------------- |
| **Protocol**    | MCP (JSON-RPC over stdio or HTTP)              |
| **Languages**   | Any (Python, TypeScript, Go, Rust, etc.)       |
| **Sandboxing**  | Process isolation only (no filesystem sandbox) |
| **Binary size** | Zero impact on host binary                     |
| **Build time**  | Zero impact on host build                      |
| **Ecosystem**   | Thousands of existing MCP servers              |

**Pros**: Zero binary impact, any language, massive ecosystem, battle-tested
**Cons**: Process spawn overhead, no host-level sandboxing, requires external runtime

### Recommendation

**Process-based (MCP) is the clear winner for ion.**

| Criterion            | Embedded JS/TS  | WASM           | Process (MCP)       |
| -------------------- | --------------- | -------------- | ------------------- |
| Binary size impact   | +30-50MB        | +5-15MB        | 0                   |
| Build time impact    | Minutes         | Moderate       | 0                   |
| Language support     | JS/TS only      | Rust/Go/C      | Any                 |
| Ecosystem            | Small           | Small          | Massive             |
| Developer experience | Good (familiar) | Poor (compile) | Good (any language) |
| Sandboxing           | Good            | Excellent      | Process-level       |
| Complexity           | High            | High           | Low                 |

Embedding V8 or Wasmtime is a significant architectural commitment for marginal benefit over MCP. The entire industry has standardized on MCP for tool extensibility. WASM could be interesting for a future "untrusted marketplace" scenario but is premature for ion.

---

## 9. Minimal Viable Extension System for ion

### Phase 1: Foundation (ship first)

| Component             | Implementation                                 | Effort |
| --------------------- | ---------------------------------------------- | ------ |
| **AGENTS.md**         | Already exists                                 | Done   |
| **Skills (SKILL.md)** | Already exists (skill/ module)                 | Done   |
| **MCP Client**        | Spawn stdio/HTTP MCP servers, route tool calls | Medium |

This gives ion: project context, reusable knowledge, and external tool access.

### Phase 2: Lifecycle Control

| Component      | Implementation                                              | Effort |
| -------------- | ----------------------------------------------------------- | ------ |
| **Hooks**      | JSON config in settings, shell commands on lifecycle events | Medium |
| **Key events** | PreToolUse, PostToolUse, Stop, SessionStart                 | Medium |
| **Matchers**   | Regex on tool names                                         | Low    |

Priority hooks: auto-format after edits, block protected files, notifications.

### Phase 3: Distribution

| Component            | Implementation                              | Effort |
| -------------------- | ------------------------------------------- | ------ |
| **Plugin manifest**  | `plugin.json` bundling skills + MCP + hooks | Low    |
| **Install command**  | `ion plugin add <git-url>`                  | Medium |
| **Plugin directory** | `~/.config/ion/plugins/` or `.ion/plugins/` | Low    |

### What NOT to Build

| Feature                | Why Skip                                              |
| ---------------------- | ----------------------------------------------------- |
| Embedded JS/TS runtime | +30-50MB binary, massive complexity, MCP handles this |
| WASM plugin host       | Premature, small ecosystem for agent plugins          |
| Custom slash commands  | Skills handle this (model-invoked > user-invoked)     |
| Extension marketplace  | Community too small, git URLs sufficient              |
| LSP server bundling    | Complex, niche use case                               |

### Architecture Sketch

```
ion
  |
  +-- AGENTS.md loader (project context, always loaded)
  |
  +-- Skill loader (SKILL.md)
  |     - Progressive: metadata at startup, full content on demand
  |     - Sources: ~/.config/ion/skills/, .ion/skills/, plugin-bundled
  |     - Tool restrictions via allowed-tools frontmatter
  |
  +-- MCP client
  |     - Config: .ion/mcp.json or ~/.config/ion/mcp.json
  |     - Transports: stdio (spawn process), HTTP/SSE
  |     - Lazy tool loading (only inject tool descriptions when relevant)
  |     - Can also expose ion as MCP server (future)
  |
  +-- Hook engine
  |     - Config: settings.json (user/project/local)
  |     - Events: PreToolUse, PostToolUse, Stop, SessionStart, etc.
  |     - Types: command (shell), prompt (LLM eval), agent (subagent)
  |     - Communication: stdin JSON -> exit code + stdout/stderr
  |
  +-- Plugin loader (future)
        - Manifest: .ion-plugin/plugin.json
        - Bundles: skills/ + .mcp.json + hooks/hooks.json
        - Install: ion plugin add <git-url>
```

---

## References

**Claude Code**:

- [Plugin announcement (Oct 2025)](https://www.claude.com/blog/claude-code-plugins)
- [Hooks guide](https://code.claude.com/docs/en/hooks-guide)
- [Plugin structure](https://claude-plugins.dev/skills/@anthropics/claude-plugins-official/plugin-structure)
- [Plugin reference (MCP Market)](https://mcpmarket.com/tools/skills/claude-code-plugin-reference-2026)

**Codex CLI**:

- [CLI reference](https://developers.openai.com/codex/cli/reference/)
- [CLI overview](https://developers.openai.com/codex/cli)

**Gemini CLI**:

- [Extensions announcement (Oct 2025)](https://blog.google/innovation-and-ai/technology/developers-tools/gemini-cli-extensions/)
- [Extensions guide (dev.to)](https://dev.to/sienna/gemini-cli-extensions-the-complete-developers-guide-to-ai-powered-command-line-customization-g2b)
- [InfoQ coverage](https://www.infoq.com/news/2025/10/gemini-cli-extensions/)

**Amp**:

- [Owner's Manual](https://ampcode.com/manual)
- [Chronicle](https://ampcode.com/chronicle)

**Cursor/Windsurf**:

- [Cursor features](https://cursor.com/features)
- [Cursor changelog](https://cursor.com/changelog)
- [Windsurf plugins](https://docs.windsurf.com/plugins/getting-started)

**MCP vs Skills**:

- [MCP context overload (EclipseSource)](https://eclipsesource.com/blogs/2026/01/22/mcp-context-overload/)
- [MCP, Skills, and Agents (cra.mr)](https://cra.mr/mcp-skills-and-agents/)
- [Why MCP if skills exist (Mintlify)](https://www.mintlify.com/blog/why-do-we-need-mcp-if-skills-exist)
- [Coding agents explained (Codeaholicguy)](https://codeaholicguy.com/2026/01/31/ai-coding-agents-explained-rules-commands-skills-mcp-hooks/)
- [Code execution with MCP (Anthropic)](https://www.anthropic.com/engineering/code-execution-with-mcp)

**Embedded JS/TS in Rust**:

- [rustyscript](https://github.com/rscarson/rustyscript)
- [deno_core](https://crates.io/crates/deno_core)
- [rusty_v8 stable announcement](https://deno.com/blog/rusty-v8-stabilized)
- [Deno open source projects](https://deno.com/blog/open-source)

**WASM Plugins**:

- [Extism plugin system](https://extism.org/docs/concepts/plug-in-system/)
- [moonrepo WASM plugins](https://moonrepo.dev/docs/guides/wasm-plugins)
- [WASM Component Model (dev.to)](https://dev.to/topheman/webassembly-component-model-building-a-plugin-system-58o0)
- [Plugins with WASI Preview 2](https://benw.is/posts/plugins-with-rust-and-wasi)

**Agent Comparisons**:

- [2026 CLI tools comparison (Tembo)](https://www.tembo.io/blog/coding-cli-tools-comparison)
