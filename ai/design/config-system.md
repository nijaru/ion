# ion Configuration System Design

**Date**: 2026-01-18
**Status**: Draft

## Overview

ion uses a 3-tier configuration hierarchy with TOML format. Universal AI agent files go in `~/.agents/` (proposed standard), ion-specific config goes in `~/.ion/`.

## Directory Structure

```
# Universal (proposed standard - preferred for shared files)
~/.agents/
├── AGENTS.md            # Global instructions (works with any AI tool)
├── skills/              # Shared skills
│   └── *.md
└── subagents/           # Subagent definitions
    └── *.md

# ion-specific
~/.ion/
├── config.toml          # ion preferences
├── AGENTS.md            # Fallback (prefer ~/.agents/AGENTS.md)
├── skills/              # Fallback (prefer ~/.agents/skills/)
└── data/
    └── sessions.db      # Session history

# Project-level (committed)
.ion/
├── config.toml          # Team settings
└── skills/              # Project skills
    └── *.md

# Project-level (local, gitignored)
.ion/
└── config.local.toml    # Personal overrides

# Root-level (universal standards)
AGENTS.md                # Project instructions (primary)
CLAUDE.md                # Project instructions (fallback)
.mcp.json                # MCP servers (compatibility)
```

## Configuration Hierarchy

**Precedence (highest to lowest)**:

1. CLI flags (runtime)
2. Environment variables
3. Local project (`.ion/config.local.toml`) - gitignored
4. Shared project (`.ion/config.toml`) - committed
5. User global (`~/.ion/config.toml`)
6. Built-in defaults

Configs are **merged**, not replaced. Later sources override only conflicting keys.

## File Format: TOML

**Rationale**:

- Rust ecosystem standard (Cargo.toml)
- Human-readable with comments
- Strong typing with serde
- Better than JSON (no comments) or YAML (whitespace issues)

## Config Schema

```toml
# ~/.ion/config.toml or .ion/config.toml

# Provider configuration
[provider]
default = "openrouter"
model = "anthropic/claude-sonnet-4"

[provider.openrouter]
api_key_env = "OPENROUTER_API_KEY"  # Reference env var, never store keys
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
thinking_level = "off"  # off, low, med, high

# Tool permissions
[permissions]
bash = "approve"    # "allow", "approve", "deny"
edit = "approve"
write = "approve"
read = "allow"
glob = "allow"
grep = "allow"

# MCP servers (merged with .mcp.json if present)
[mcp.servers.github]
command = "npx"
args = ["-y", "@anthropic/mcp-server-github"]
env = { GITHUB_TOKEN = "${GITHUB_TOKEN}" }

# Session management
[session]
auto_save = true
retention_days = 30
```

## Instruction Files

### Loading Order

1. `./AGENTS.md` (project root, primary standard)
2. `./CLAUDE.md` (project root, fallback for Claude Code compat)
3. `~/.agents/AGENTS.md` (user global, preferred)
4. `~/.ion/AGENTS.md` (user global, fallback)

At each level, first found wins. Final result is project file + user file (max 2 files).

**Not supported** (users can rename to AGENTS.md):

- `.cursorrules` - deprecated by Cursor
- `.cursor/rules/*.mdc` - complex format, low ROI
- `.goosehints`, `.clinerules` - minimal adoption

### Subdirectory Discovery

When working in a subdirectory, walk up to find nearest instruction file:

```
src/components/Button.tsx  # Working here
src/components/AGENTS.md   # Check first
src/AGENTS.md              # Then here
./AGENTS.md                # Then root
```

## Skills

### Loading Order

1. `.ion/skills/*.md` (project skills)
2. `~/.agents/skills/*.md` (user skills, preferred)
3. `~/.ion/skills/*.md` (user skills, fallback)
4. Built-in skills

Skills from all locations are merged (not overwritten). Project skills can override user skills by name.

### Format

Markdown files with optional YAML frontmatter:

```markdown
---
name: rust-patterns
description: Rust idioms and patterns
---

# Rust Patterns

Prefer `&str` over `String` for function parameters...
```

## MCP Configuration

### Primary: config.toml

```toml
[mcp.servers.filesystem]
command = "npx"
args = ["-y", "@anthropic/mcp-server-filesystem", "/path"]

[mcp.servers.github]
command = "npx"
args = ["-y", "@anthropic/mcp-server-github"]
env = { GITHUB_TOKEN = "${GITHUB_TOKEN}" }
```

### Compatibility: .mcp.json

Also load `.mcp.json` (Claude Desktop format) if present:

```json
{
  "mcpServers": {
    "github": {
      "command": "npx",
      "args": ["-y", "@anthropic/mcp-server-github"]
    }
  }
}
```

Servers from both sources are merged.

## Secrets Handling

**Never store API keys in config files.**

### Recommended Pattern

```toml
[provider.openrouter]
api_key_env = "OPENROUTER_API_KEY"  # Reference env var name
```

### Environment Variable Expansion

Support `${VAR}` syntax:

```toml
[mcp.servers.github]
env = { GITHUB_TOKEN = "${GITHUB_TOKEN}" }
```

### Fallback: Keyring

Future: integrate with system keyring for secure storage.

## Auto-Gitignore

When `.ion/config.local.toml` is first created:

1. Check if `.gitignore` exists in project root
2. If exists, check if `.ion/*.local.toml` is already listed
3. If not listed, append:
   ```
   # ion local config
   .ion/*.local.toml
   ```

## Migration from Current

### Current Structure

```
~/.config/ion/config.toml      # via directories crate
~/.local/share/ion/sessions.db
```

### New Structure

```
~/.ion/config.toml
~/.ion/data/sessions.db
```

### Migration Steps

1. On first run with new version:
   - Check for old config at `~/.config/ion/`
   - If found, copy to `~/.ion/`
   - Log migration message
   - Old location remains (user can delete)

## Implementation

### Crates

| Crate    | Purpose                                     |
| -------- | ------------------------------------------- |
| `config` | Layered config loading                      |
| `toml`   | TOML parsing (already have)                 |
| `dirs`   | Home directory (simpler than `directories`) |

### Config Loading

```rust
pub struct ConfigLoader {
    user_path: PathBuf,      // ~/.ion/config.toml
    project_path: PathBuf,   // .ion/config.toml
    local_path: PathBuf,     // .ion/config.local.toml
}

impl ConfigLoader {
    pub fn load() -> Result<Config> {
        let mut builder = config::Config::builder();

        // Layer 1: Defaults
        builder = builder.add_source(config::File::from_str(
            include_str!("defaults.toml"),
            config::FileFormat::Toml
        ));

        // Layer 2: User global
        if self.user_path.exists() {
            builder = builder.add_source(
                config::File::from(self.user_path.clone())
            );
        }

        // Layer 3: Project shared
        if self.project_path.exists() {
            builder = builder.add_source(
                config::File::from(self.project_path.clone())
            );
        }

        // Layer 4: Project local
        if self.local_path.exists() {
            builder = builder.add_source(
                config::File::from(self.local_path.clone())
            );
        }

        // Layer 5: Environment
        builder = builder.add_source(
            config::Environment::with_prefix("ION")
        );

        builder.build()?.try_deserialize()
    }
}
```

### Instructions Loading

```rust
pub fn load_instructions(working_dir: &Path) -> String {
    let mut instructions = String::new();

    // Project root
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let path = working_dir.join(name);
        if path.exists() {
            instructions.push_str(&format!("# From {}\n\n", name));
            instructions.push_str(&fs::read_to_string(&path).unwrap_or_default());
            instructions.push_str("\n\n");
        }
    }

    // User global
    let user_agents = dirs::home_dir()
        .map(|h| h.join(".ion/AGENTS.md"));
    if let Some(path) = user_agents {
        if path.exists() {
            instructions.push_str("# From ~/.ion/AGENTS.md\n\n");
            instructions.push_str(&fs::read_to_string(&path).unwrap_or_default());
        }
    }

    instructions
}
```

## Tasks

- [ ] Replace `directories` crate with `dirs` (simpler)
- [ ] Change config path from `~/.config/ion/` to `~/.ion/`
- [ ] Add AGENTS.md loading to ContextManager
- [ ] Add CLAUDE.md fallback
- [ ] Implement config.local.toml support
- [ ] Add auto-gitignore for local config
- [ ] Add migration from old config location
- [ ] Update README with new config location

## ~/.agents/ Standard Proposal

ion proposes `~/.agents/` as a universal user-level location for AI agent files:

```
~/.agents/
├── AGENTS.md            # Global instructions
├── skills/              # Shared skills (markdown)
└── subagents/           # Subagent definitions
```

**Rationale**:

- `AGENTS.md` is becoming the project-level standard (60k+ repos)
- No user-level standard currently exists
- `~/.agents/` is the logical user-level counterpart
- Tool-agnostic: works with any AI coding tool

**Adoption path**:

1. ion supports it natively
2. Document the convention
3. Encourage other tools to adopt
4. Users benefit from single source of truth

Other tools currently use tool-specific paths (`~/.claude/`, `~/.cursor/`). A universal location would reduce duplication and let users maintain one set of instructions.

## References

- Research: `ai/research/cli-agent-config-best-practices.md`
- AGENTS.md standard: https://agents.md/
- Claude Code settings: Reference implementation
