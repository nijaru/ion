# Skill/Config Directory Conventions for Coding Agents (February 2026)

**Date**: 2026-02-19
**Purpose**: Map where each major coding agent looks for skills, config, and instructions
**Status**: Complete

---

## Executive Summary

| Agent       | User Config Dir       | Project Config Dir   | User Skills                  | Project Skills      | Instruction File   |
| ----------- | --------------------- | -------------------- | ---------------------------- | ------------------- | ------------------ |
| Claude Code | `~/.claude/`          | `.claude/`           | `~/.claude/skills/*/`        | `.claude/skills/*/` | CLAUDE.md          |
| Gemini CLI  | `~/.gemini/`          | `.gemini/`           | N/A (extensions instead)     | N/A                 | GEMINI.md          |
| Codex CLI   | `~/.codex/`           | `.codex/`            | Undocumented                 | Undocumented        | AGENTS.md          |
| Amp         | `~/.config/amp/`      | `.amp/` + `.agents/` | `~/.config/agents/skills/`   | `.agents/skills/*/` | AGENTS.md          |
| OpenCode    | `~/.config/opencode/` | `.opencode/`         | `~/.config/opencode/skills/` | `.opencode/skills/` | AGENTS.md (config) |
| Cursor      | `~/.cursor/`          | `.cursor/`           | N/A                          | N/A                 | .cursorrules       |

**Key finding**: There is no universal standard for user-level config directories. Every tool uses its own path. The agentskills.io spec does not define discovery locations -- it only specifies the skill directory/file format.

---

## 1. agentskills.io Specification

The spec (https://agentskills.io/specification) defines **format only**, not discovery locations.

### Skill Directory Structure

```
skill-name/
  SKILL.md          # Required
  scripts/          # Optional: executable code
  references/       # Optional: additional docs
  assets/           # Optional: templates, images, data
```

### SKILL.md Format

YAML frontmatter + Markdown body:

```yaml
---
name: skill-name # Required: 1-64 chars, lowercase + hyphens, must match dir name
description: What it does # Required: 1-1024 chars
license: Apache-2.0 # Optional
compatibility: ... # Optional: env requirements
metadata: { ... } # Optional: arbitrary k/v
allowed-tools: Bash Read # Optional, experimental: space-delimited tool list
---
Markdown instructions here...
```

### Progressive Disclosure

1. **Metadata** (~100 tokens): name + description loaded at startup for all skills
2. **Instructions** (<5000 tokens recommended): full SKILL.md loaded on activation
3. **Resources** (as needed): scripts/, references/, assets/ loaded on demand

### What the Spec Does NOT Define

- Where agents should discover skills (user-level, project-level paths)
- How skills are activated (auto-match vs manual invoke)
- Runtime behavior (sandboxing, tool restrictions enforcement)
- Versioning or dependency resolution between skills

Each agent implementation decides these independently.

---

## 2. Claude Code Directory Structure

### User-Level

```
~/.claude/
  CLAUDE.md                    # User instructions (always loaded)
  settings.json                # User preferences
  skills/                      # User skills
    skill-name/
      SKILL.md
      scripts/
  commands/                    # Slash commands
    command-name.md
  agents/                      # Subagent definitions
    agent-name.md
```

### Project-Level

```
.claude/
  settings.json                # Project config (committed)
  settings.local.json          # Local overrides (gitignored)
  .mcp.json                    # MCP servers
  skills/                      # Project skills
    skill-name/
      SKILL.md
  commands/                    # Project slash commands
  agents/                      # Project subagent definitions
  rules/                       # Path-specific rules
  hooks/                       # Lifecycle hooks (in settings)

CLAUDE.md                      # Project instructions (root)
```

### Skill Discovery Order

1. `.claude/skills/*/SKILL.md` (project)
2. `~/.claude/skills/*/SKILL.md` (user)
3. Plugin-bundled skills

Skills use progressive disclosure: only name + description loaded at startup. Full SKILL.md loaded when the model decides the skill is relevant to the task.

### Key Details

- Claude Code does NOT use XDG paths -- it uses `~/.claude/` directly
- CLAUDE.md is also recognized as AGENTS.md (recently added compatibility)
- Enterprise managed settings: `/Library/Application Support/ClaudeCode/` (macOS)
- Skills are model-invoked (Claude decides when to load them based on description match)
- User confirmation required before skill activation

---

## 3. Gemini CLI Directory Structure

### User-Level

```
~/.gemini/
  settings.json                # User config
  GEMINI.md                    # User instructions (global)
```

### Project-Level

```
.gemini/
  settings.json                # Project config
  GEMINI.md                    # Project instructions

GEMINI.md                      # Root-level instructions (also recognized)
```

### Config Precedence (lowest to highest)

1. Default values
2. User settings: `~/.gemini/settings.json`
3. Project settings: `.gemini/settings.json`
4. Environment variables
5. Command-line arguments

### Skills/Extensions

Gemini CLI does not have a standalone skill system. It uses "extensions" which are bundled MCP servers + context:

```
my-extension/
  gemini-extension.json        # Manifest
  GEMINI.md                    # Extension-specific instructions
  mcp-servers/                 # Bundled MCP server code
  commands/                    # Custom slash commands
```

No separate user/project skill directories.

---

## 4. OpenAI Codex CLI Directory Structure

### User-Level

```
~/.codex/
  config.toml                  # User config (TOML, not JSON)
  instructions.md              # User instructions
```

### Project-Level

```
.codex/
  config.toml                  # Project config (requires trust)

AGENTS.md                      # Project instructions (root)
```

### System-Level

```
/etc/codex/config.toml         # Unix system config
```

### Config Precedence (highest to lowest)

1. CLI flags and `--config` overrides
2. Profile values
3. Project config: `.codex/config.toml` (root to cwd, deepest wins)
4. User config: `~/.codex/config.toml`
5. System config: `/etc/codex/config.toml`
6. Built-in defaults

### Skills

Codex CLI mentions skills but they are minimally documented. Primary extensibility is through AGENTS.md and MCP (configured in config.toml).

---

## 5. Amp (Sourcegraph) Directory Structure

### User-Level

```
~/.config/amp/
  settings.json                # User config
  AGENTS.md                    # User instructions (also checks ~/.config/AGENTS.md)
  skills/                      # User skills (also checks ~/.config/agents/skills/)

~/.config/agents/
  skills/                      # Cross-agent user skills (proposed standard)
```

### Project-Level

```
.amp/
  settings.json                # Project config (workspace settings)

.agents/
  skills/                      # Project skills (shared convention)
    skill-name/
      SKILL.md
  checks/                      # Review criteria
    check-name.md

AGENTS.md                      # Project instructions (root)
```

### Key Details

- Amp recognizes AGENTS.md, AGENT.md, and CLAUDE.md as instruction file names
- AGENTS.md files are discovered walking up from cwd to $HOME
- Skills in `.agents/skills/` follow the agentskills.io spec
- `~/.config/agents/skills/` is a user-wide skill location (cross-tool convention)
- Amp is the only CLI agent using `~/.config/` style paths (partial XDG)
- Enterprise managed settings: `/Library/Application Support/ampcode/managed-settings.json` (macOS)

---

## 6. OpenCode Directory Structure

### User-Level

```
~/.config/opencode/
  opencode.json                # User config (JSON/JSONC)
  agents/                      # User agent definitions
  commands/                    # User commands
  skills/                      # User skills
  tools/                       # User tools
  themes/                      # User themes
  plugins/                     # User plugins
  modes/                       # User modes
```

### Project-Level

```
opencode.json                  # Project config (root)

.opencode/
  agents/                      # Project agents
  commands/                    # Project commands
  skills/                      # Project skills
  tools/                       # Project tools
  plugins/                     # Project plugins
  modes/                       # Project modes
  themes/                      # Project themes
```

### Config Precedence (lowest to highest)

1. Remote config (`.well-known/opencode` endpoint)
2. Global config (`~/.config/opencode/opencode.json`)
3. Custom config (`OPENCODE_CONFIG` env var)
4. Project config (`opencode.json`)
5. `.opencode/` directories
6. Inline config (`OPENCODE_CONFIG_CONTENT` env var)

### Key Details

- OpenCode follows XDG convention (`~/.config/opencode/`)
- NOTE: opencode-ai/opencode repo was archived September 2025
- A newer opencode.ai exists with active development
- Uses plural directory names (`agents/`, `skills/`, `commands/`) with singular backward compat

---

## 7. XDG vs App-Specific Directory Convention

### XDG Base Directory Specification

```
$XDG_CONFIG_HOME/app/    # Default: ~/.config/app/
$XDG_DATA_HOME/app/      # Default: ~/.local/share/app/
$XDG_CACHE_HOME/app/     # Default: ~/.cache/app/
$XDG_STATE_HOME/app/     # Default: ~/.local/state/app/
```

### CLI Tool Convention on macOS

Research shows CLI tools have converged on using `~/.config/` on macOS, NOT `~/Library/Application Support/`:

**Tools using `~/.config/` on macOS**: gh, git, packer, stripe, op (1Password), kubectl, docker, terraform, ruff, amp, opencode

**Tools using app-specific `~/.<name>/`**: Claude Code (`~/.claude/`), Gemini CLI (`~/.gemini/`), Codex CLI (`~/.codex/`), Cursor (`~/.cursor/`)

### Pattern Analysis

| Approach                  | Examples                           | Pros                                             | Cons                                   |
| ------------------------- | ---------------------------------- | ------------------------------------------------ | -------------------------------------- |
| `~/.config/ion/`          | amp, opencode, gh, docker          | XDG standard, consistent cross-platform          | Longer path, less discoverable         |
| `~/.ion/`                 | (ion current design)               | Short, discoverable, matches Cargo (`~/.cargo/`) | Non-standard, clutters $HOME           |
| `~/.claude/`, `~/.codex/` | Claude Code, Codex, Gemini, Cursor | Short, brand-visible, easy to find               | Non-standard, every tool adds a dotdir |
| `~/.agents/`              | (ion proposed universal)           | Tool-agnostic, single source of truth            | Doesn't exist yet, adoption uncertain  |

### What Rust CLI Tools Do

- **Cargo**: `~/.cargo/` (app-specific, predates modern XDG convention)
- **Rustup**: `~/.rustup/`
- **ripgrep**: `~/.config/ripgrep/` (XDG)
- **starship**: `~/.config/starship.toml` (XDG)
- **nushell**: `~/.config/nushell/` (XDG)
- **helix**: `~/.config/helix/` (XDG)
- **alacritty**: `~/.config/alacritty/` (XDG)
- **zellij**: `~/.config/zellij/` (XDG)

Modern Rust CLI tools overwhelmingly use `~/.config/`. The Cargo/Rustup exceptions are historical.

---

## 8. Recommendation for ion

### Decision: Use `~/.config/ion/` (XDG)

**Rationale**:

1. Modern Rust CLI tools use `~/.config/` (helix, zellij, starship, nushell, alacritty)
2. Cross-platform consistency (same path Linux + macOS)
3. Respects `$XDG_CONFIG_HOME` override
4. Data separation: config in `~/.config/`, data in `~/.local/share/`, cache in `~/.cache/`
5. The `~/.ion/` approach from the config-system.md design doc conflates config, data, and cache

The `~/.claude/` pattern (app-specific dotdir) is a branding choice by Anthropic/Google/OpenAI. It works for dominant products but is not the right pattern for a tool that wants to be a good Unix citizen.

### Proposed Directory Layout

```
# User-level config (XDG)
~/.config/ion/
  config.toml              # User preferences
  skills/                  # User skills (agentskills.io format)
    skill-name/
      SKILL.md
  agents/                  # Subagent definitions

# User-level data (XDG)
~/.local/share/ion/
  sessions.db              # Session database
  model-cache/             # Model list cache

# User-level cache (XDG)
~/.cache/ion/
  ...

# User instructions (multiple locations checked)
~/.config/ion/AGENTS.md         # ion-specific user instructions
~/.config/agents/AGENTS.md      # Cross-tool user instructions (future convention)

# Project-level
.ion/
  config.toml              # Project config (committed)
  config.local.toml        # Local overrides (gitignored)
  skills/                  # Project skills
    skill-name/
      SKILL.md

# Root-level (universal standards)
AGENTS.md                  # Project instructions (primary)
CLAUDE.md                  # Project instructions (fallback, compatibility)
.mcp.json                  # MCP servers (compatibility with Claude Code)
```

### Skills Discovery Order

1. `.ion/skills/*/SKILL.md` (project skills)
2. `~/.config/ion/skills/*/SKILL.md` (user skills)
3. `~/.config/agents/skills/*/SKILL.md` (cross-tool user skills, future)
4. Built-in skills

Project skills override user skills of the same name. All locations merged.

### Instructions Discovery Order

1. `./AGENTS.md` (project root, primary)
2. `./CLAUDE.md` (project root, fallback for Claude Code compat)
3. Subdirectory `AGENTS.md` walk-up (when working in subdir)
4. `~/.config/ion/AGENTS.md` (user-level)

### Compatibility with agentskills.io

ion already follows the agentskills.io spec in `src/skill/mod.rs`:

- YAML frontmatter with name + description
- Progressive disclosure (summary at startup, full on demand)
- Subdirectory pattern (`skill-name/SKILL.md`)
- Also supports standalone `.md` files in skills dir

### What Changed from config-system.md Design

| Aspect            | config-system.md (Jan 2026)          | This Recommendation               |
| ----------------- | ------------------------------------ | --------------------------------- |
| User config       | `~/.ion/config.toml`                 | `~/.config/ion/config.toml`       |
| User data         | `~/.ion/data/sessions.db`            | `~/.local/share/ion/sessions.db`  |
| Universal dir     | `~/.agents/` (proposed new standard) | `~/.config/agents/` (XDG-aligned) |
| Fallback skills   | `~/.ion/skills/`                     | No fallback needed                |
| Fallback instruct | `~/.ion/AGENTS.md`                   | No fallback needed                |

The `~/.agents/` proposal from config-system.md was well-intentioned but faces the same adoption problem as any new standard. Using `~/.config/agents/` is more likely to succeed because it fits within established conventions.

---

## References

- agentskills.io spec: https://agentskills.io/specification
- Claude Code docs: https://docs.claude.com/en/docs/agents-and-tools/agent-skills/overview
- Gemini CLI config: https://geminicli.com/docs/get-started/configuration/
- Codex CLI config: https://developers.openai.com/codex/config-basic/
- Amp manual: https://ampcode.com/manual
- OpenCode config: https://opencode.ai/docs/config/
- XDG spec: https://specifications.freedesktop.org/basedir-spec/latest/
- CLI tools XDG on macOS: https://atmos.tools/changelog/macos-xdg-cli-conventions
- Prior ion research: `ai/research/cli-agent-config-best-practices.md`
- Prior ion design: `ai/design/config-system.md`
- Prior ion research: `ai/research/extensibility-systems-2026.md`
