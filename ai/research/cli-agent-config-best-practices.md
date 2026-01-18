# CLI Coding Agent Configuration Best Practices

**Date**: 2026-01-18
**Purpose**: Research configuration patterns for ion TUI agent

---

## Executive Summary

Modern CLI coding agents converge on a **4-tier hierarchy** with JSON/TOML config + Markdown instructions. Key findings:

| Pattern       | Industry Standard                              | Recommendation            |
| ------------- | ---------------------------------------------- | ------------------------- |
| Config format | JSON (Claude, OpenCode) or YAML (Goose, aider) | **TOML** (Rust ecosystem) |
| Hierarchy     | Enterprise > User > Project > Local            | Adopt 4-tier              |
| Instructions  | AGENTS.md at root (60k+ repos)                 | AGENTS.md + symlink       |
| MCP           | Separate .mcp.json                             | Merge into main config    |
| Secrets       | .local suffix, gitignored                      | .local.toml pattern       |

---

## 1. Configuration Hierarchy

### Claude Code (Reference Implementation)

**Precedence (highest to lowest)**:

1. **Managed** - `/Library/Application Support/ClaudeCode/` (enterprise policy)
2. **CLI flags** - Runtime overrides
3. **Local project** - `.claude/settings.local.json` (gitignored)
4. **Shared project** - `.claude/settings.json` (committed)
5. **User** - `~/.claude/settings.json`
6. **Defaults** - Built-in

**File locations**:

```
~/.claude/settings.json          # User preferences
~/.claude/CLAUDE.md              # User instructions
.claude/settings.json            # Project config (committed)
.claude/settings.local.json      # Local overrides (gitignored)
CLAUDE.md                        # Project instructions (committed)
CLAUDE.local.md                  # Local instructions (deprecated)
```

### OpenCode

**Precedence**:

1. Remote config (`.well-known/opencode`) - organizational defaults
2. Global config (`~/.config/opencode/opencode.json`)
3. Custom config (`OPENCODE_CONFIG` env var)
4. Project config (`opencode.json` in project root)
5. `.opencode/` directories - agents, commands, plugins

**Key insight**: Configs are **merged**, not replaced. Later sources override conflicting keys only.

### Goose

**Single user-level config**:

- macOS/Linux: `~/.config/goose/config.yaml`
- Windows: `%APPDATA%\Block\goose\config\config.yaml`

**Separate files for different concerns**:

- `config.yaml` - Provider, model, extensions
- `permission.yaml` - Tool permission levels
- `secrets.yaml` - API keys (only when keyring disabled)
- `permissions/tool_permissions.json` - Runtime decisions (auto-managed)

### Aider

**YAML with layered loading**:

1. `~/.aider.conf.yml` (home directory)
2. `<git-root>/.aider.conf.yml` (repo root)
3. `./.aider.conf.yml` (current directory)

Files loaded in order; later files override.

### Cursor

**New system (v0.45+)**:

- `.cursor/rules/*.mdc` - MDC files (Markdown with YAML frontmatter)
- Legacy: `.cursorrules` (deprecated)

**MDC format**:

```yaml
---
description: Short description of the rule's purpose
globs: src/**/*.ts
alwaysApply: false
---
# Rule Title
Main rule content with instructions...
```

---

## 2. File Format Comparison

### TOML (Recommended for Rust)

**Pros**:

- Rust ecosystem standard (Cargo.toml)
- Human-readable and writable
- Strong typing with clear syntax
- Good editor support
- Native serde support

**Cons**:

- Less familiar to JS/Python developers
- Nested structures less elegant than YAML

**Rust crates**:

- `config-rs` - Layered config with TOML/JSON/YAML support
- `figment` - Configuration library (used by Rocket)
- `directories` - XDG-compliant paths

### JSON (Used by Claude Code, OpenCode)

**Pros**:

- Universal familiarity
- Schema validation (JSON Schema)
- JSONC variant allows comments

**Cons**:

- No comments in standard JSON
- Verbose for nested structures
- Trailing comma errors

### YAML (Used by Goose, aider)

**Pros**:

- Human-readable
- Good for complex nested structures
- Supports comments

**Cons**:

- Whitespace-sensitive (error-prone)
- Security concerns (arbitrary code execution in some parsers)
- Inconsistent across parsers

### Recommendation for ion

**Use TOML** because:

1. Rust ecosystem convention (Cargo.toml)
2. Human-readable config + comments
3. Strong typing aligns with Rust
4. `config-rs` provides layered loading

**Example schema**:

```toml
# ~/.config/ion/config.toml

[provider]
default = "openrouter"
model = "anthropic/claude-sonnet-4"

[provider.openrouter]
api_key_env = "OPENROUTER_API_KEY"  # Don't store actual keys

[tui]
theme = "dark"
scroll_speed = 3

[permissions]
bash = "approve"      # "allow", "approve", "deny"
edit = "approve"
write = "approve"
```

---

## 3. Gitignore Conventions

### Standard Patterns

**Committed** (team-shared):

- `.ion/config.toml` - Project settings
- `AGENTS.md` - Agent instructions
- `.mcp.json` - MCP servers (if no secrets)

**Gitignored** (personal/secrets):

- `.ion/config.local.toml` - Local overrides
- `.ion/secrets.toml` - API keys (if not using keyring)
- `.env.local` - Environment overrides

### Auto-gitignore

Claude Code **automatically** adds `.claude/settings.local.json` to `.gitignore` when created.

**Recommendation**: ion should auto-add `.ion/*.local.toml` to `.gitignore` on first creation.

### Naming Conventions

| Suffix     | Purpose            | Gitignored |
| ---------- | ------------------ | ---------- |
| `.local`   | Personal overrides | Yes        |
| `.user`    | User-specific      | Yes        |
| `.dev`     | Development only   | Usually    |
| `.example` | Template           | No         |
| `.sample`  | Template           | No         |

### Secrets Handling

**Best practices**:

1. **Never store secrets in config files** - Use environment variables or keyring
2. **api_key_env** pattern - Reference env var name, not value
3. **Separate secrets file** - `secrets.toml` if keyring unavailable
4. **.env.example** - Template showing required vars without values

**Example**:

```toml
# config.toml (committed)
[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"  # Reference, not value

# .env.example (committed)
# ANTHROPIC_API_KEY=your-key-here
# OPENROUTER_API_KEY=your-key-here
```

---

## 4. AGENTS.md / Instruction Files

### Growing Standard

**AGENTS.md** has emerged as the open standard for AI coding agent instructions:

- **60,000+ open-source projects** use it
- Supported by: Claude Code, OpenCode, Cursor, Codex, Aider, Goose, Gemini CLI, Zed

**Key principle**: "README for agents" - dedicated place for context that coding agents need.

### Hierarchy and Discovery

**Claude Code**:

1. Enterprise: `/etc/claude-code/CLAUDE.md`
2. User: `~/.claude/CLAUDE.md`
3. Project: `./CLAUDE.md` or `./.claude/CLAUDE.md`
4. Local: `./CLAUDE.local.md` (deprecated)
5. Nested: `src/components/CLAUDE.md` (auto-loaded with @-mentions)

**OpenCode**:

- `~/.config/opencode/` directory
- `AGENTS.md` via `instructions` config option

### Naming Conventions

| File                  | Tool        | Status                   |
| --------------------- | ----------- | ------------------------ |
| `AGENTS.md`           | Universal   | **Standard**             |
| `CLAUDE.md`           | Claude Code | Tool-specific            |
| `.cursorrules`        | Cursor      | Deprecated               |
| `.cursor/rules/*.mdc` | Cursor      | Current                  |
| `.aider.conf.yml`     | aider       | Config, not instructions |

### Recommendation for ion

**Support both**:

```
AGENTS.md           # Primary (universal standard)
.ion/CLAUDE.md      # Symlink to AGENTS.md for Claude Code compatibility
```

**Discovery order**:

1. `./AGENTS.md` (project root)
2. `./.ion/AGENTS.md` (config directory)
3. `~/.config/ion/AGENTS.md` (user global)

**Auto-discovery in subdirectories**: Load nearest AGENTS.md when working in a subproject.

### Content Guidelines

Based on analysis of 2,500+ repos (GitHub blog):

**Essential sections**:

```markdown
# AGENTS.md

## Project Overview

One-paragraph description of what this is.

## Setup Commands

- Install deps: `cargo build`
- Run tests: `cargo test`
- Format: `cargo fmt`

## Code Style

- Rust edition 2024
- Use `anyhow` for errors
- Prefer `&str` over `String`

## Testing Requirements

- Unit tests required for new functions
- Integration tests for API changes

## Security Considerations

- Never commit API keys
- Sanitize user input
```

---

## 5. MCP Configuration

### Current Conventions

**Claude Code**:

- User scope: `~/.claude.json` (contains MCP servers)
- Project scope: `.mcp.json` (separate file)
- Format: JSON with servers array

**Example `.mcp.json`**:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@anthropic/mcp-server-filesystem", "/path/to/dir"],
      "env": {}
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@anthropic/mcp-server-github"],
      "env": {
        "GITHUB_TOKEN": "${GITHUB_TOKEN}"
      }
    }
  }
}
```

**OpenCode**:

- MCP configured in main `opencode.json` under `mcp` key
- Merged approach (not separate file)

### Recommendation for ion

**Merge into main config** (like OpenCode):

```toml
# config.toml
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@anthropic/mcp-server-filesystem", "/path"]

[mcp.servers.github]
command = "npx"
args = ["-y", "@anthropic/mcp-server-github"]
env = { GITHUB_TOKEN = "${GITHUB_TOKEN}" }
```

**Rationale**:

- Single config file is easier to manage
- TOML supports complex nested structures
- Can still support `.mcp.json` for compatibility

---

## 6. Skills/Plugins

### Claude Code Skills

**Location**: `~/.claude/skills/` and `.claude/skills/`

**Format**: Markdown files (SKILL.md)

- Not executable code
- Expanded to detailed instructions
- Injected as user messages into conversation

**Discovery**: Loaded on demand via `/skill` command or auto-discovery.

### OpenCode Plugins

**Location**: `~/.config/opencode/plugins/` and `.opencode/plugins/`

**Types**:

- Markdown-based agents/commands
- npm packages via config

### Recommendation for ion

**Directory structure**:

```
~/.config/ion/
  config.toml           # User config
  AGENTS.md             # User instructions
  skills/               # User skills
    rust-patterns.md
    testing.md
  plugins/              # User plugins (future)

.ion/
  config.toml           # Project config (committed)
  config.local.toml     # Local overrides (gitignored)
  skills/               # Project skills
```

**Loading order**:

1. Project skills (`.ion/skills/`)
2. User skills (`~/.config/ion/skills/`)
3. Built-in skills

---

## 7. Concrete Implementation for ion

### Directory Structure

```
# User-level (XDG compliant)
~/.config/ion/
  config.toml           # User preferences
  AGENTS.md             # User instructions
  skills/               # User skills
  sessions/             # Session database (rusqlite)

# Project-level
.ion/
  config.toml           # Project config (committed)
  config.local.toml     # Local overrides (auto-gitignored)

# Root-level (universal)
AGENTS.md               # Project instructions (standard)
.mcp.json               # MCP servers (optional, for compatibility)
```

### Config Schema

```toml
# .ion/config.toml

# Provider configuration
[provider]
default = "openrouter"
model = "anthropic/claude-sonnet-4"

[provider.openrouter]
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"

[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[provider.ollama]
base_url = "http://localhost:11434"
model = "qwen3:32b"

# TUI settings
[tui]
theme = "dark"
scroll_speed = 3

# Tool permissions
[permissions]
bash = "approve"    # "allow", "approve", "deny"
edit = "approve"
write = "approve"
read = "allow"
glob = "allow"
grep = "allow"

# MCP servers
[mcp.servers.github]
command = "npx"
args = ["-y", "@anthropic/mcp-server-github"]

# Session management
[session]
auto_save = true
cleanup_days = 30
```

### Auto-gitignore Implementation

On first creation of `.ion/config.local.toml`:

1. Check if `.gitignore` exists
2. If not, create it
3. Append `.ion/*.local.toml` if not present
4. Append `.ion/sessions/` if not present

### Environment Variable Expansion

Support `${VAR}` syntax in config:

```toml
[mcp.servers.github]
env = { GITHUB_TOKEN = "${GITHUB_TOKEN}" }
```

### Validation

Use JSON Schema or Rust serde validation:

- Warn on unknown fields
- Error on invalid types
- Suggest corrections (like `claude doctor`)

---

## References

**Tools analyzed**:

- Claude Code: https://code.claude.com/docs/en/settings
- OpenCode: https://opencode.ai/docs/config/
- Goose: https://block.github.io/goose/docs/guides/config-file/
- aider: https://aider.chat/docs/config/aider_conf.html
- Cursor: https://cursor.fan/tutorial/HowTo/best-practices-for-cursor-rules

**Standards**:

- AGENTS.md: https://agents.md/
- MCP: https://modelcontextprotocol.io/

**Rust ecosystem**:

- config-rs: https://github.com/rust-cli/config-rs
- figment: https://docs.rs/figment/
- directories: https://docs.rs/directories/

---

## Action Items for ion

1. **Implement 4-tier config hierarchy**:
   - Managed (future): `/etc/ion/config.toml`
   - User: `~/.config/ion/config.toml`
   - Project: `.ion/config.toml`
   - Local: `.ion/config.local.toml`

2. **Use TOML format** with `config-rs` crate

3. **Support AGENTS.md** at project root with auto-discovery

4. **Auto-gitignore** local config files

5. **MCP in main config** with `.mcp.json` compatibility layer

6. **Skills directory** at `~/.config/ion/skills/` and `.ion/skills/`
