# Context & System Prompt Design

## Overview

Ion needs a flexible system for building the system prompt that includes:

1. Base agent instructions
2. Project-specific context from markdown files
3. Active skills

## Current State

```rust
// Hardcoded in src/agent/mod.rs:120
"You are ion, a fast terminal coding agent. Be concise and efficient. Use tools to fulfill user requests."
```

No support for:

- Project-level instructions (AGENTS.md, CLAUDE.md)
- User-level global instructions
- Custom base prompts

## Design Goals

1. **Compatibility**: Support common agent instruction files (AGENTS.md, CLAUDE.md)
2. **Layering**: Clear precedence for multiple instruction sources
3. **Performance**: Cache rendered prompts, invalidate on file change
4. **Simplicity**: No complex resolution rules

## Proposed Architecture

### Instruction File Resolution

**Hierarchy (all layers combined, highest priority first):**

```
1. Project:    ./AGENTS.md or ./CLAUDE.md
2. Global:     ~/.config/agents/AGENTS.md (cross-agent standard)
3. Ion-local:  ~/.ion/AGENTS.md (ion-specific overrides)
```

**Rationale:**

- `~/.config/agents/` = XDG-compliant cross-agent standard
  - See: github.com/nijaru/global-agents-config
  - See: github.com/agentsmd/agents.md/issues/91
  - Respects `$XDG_CONFIG_HOME` on Linux
  - Windows: `%APPDATA%\agents\`
- `~/.ion/` = ion-specific config, easy to backup/transfer
- Project-level wins for per-project customization

**Layer Behavior:**

- Layers are ADDITIVE (all found files are included)
- Project instructions appear LAST (closest to conversation = most relevant)
- Conflicts: later layers can override earlier (project > global > ion-local)

### System Prompt Assembly

```
┌─────────────────────────────────────┐
│ Base Instructions (hardcoded)       │  ~50 tokens
├─────────────────────────────────────┤
│ ~/.ion/AGENTS.md (ion-specific)     │  0-1k tokens
├─────────────────────────────────────┤
│ ~/.config/agents/AGENTS.md (global) │  0-2k tokens
├─────────────────────────────────────┤
│ ./AGENTS.md or ./CLAUDE.md (project)│  0-5k tokens
├─────────────────────────────────────┤
│ Active Skill (if any)               │  0-2k tokens
├─────────────────────────────────────┤
│ Active Plan (if any)                │  0-500 tokens
└─────────────────────────────────────┘
```

**Order rationale:** LLMs have recency bias - put most specific/relevant context last.

### Implementation

#### New Module: `src/agent/instructions.rs`

```rust
pub struct InstructionLoader {
    project_path: PathBuf,
    cache: Mutex<HashMap<PathBuf, CachedFile>>,
}

struct CachedFile {
    content: String,
    mtime: SystemTime,
}

impl InstructionLoader {
    /// Load all instruction layers, returning combined content
    pub fn load_all(&self) -> String {
        let mut parts = Vec::new();

        // 1. Ion-specific (~/.ion/AGENTS.md)
        if let Some(content) = self.load_ion_local() {
            parts.push(content);
        }

        // 2. Global standard (~/.config/agents/AGENTS.md)
        if let Some(content) = self.load_global() {
            parts.push(content);
        }

        // 3. Project-level (./AGENTS.md or ./CLAUDE.md)
        if let Some(content) = self.load_project() {
            parts.push(content);
        }

        parts.join("\n\n---\n\n")
    }

    fn load_ion_local(&self) -> Option<String> {
        let path = dirs::home_dir()?.join(".ion/AGENTS.md");
        self.load_cached(&path)
    }

    fn load_global(&self) -> Option<String> {
        // Respect $XDG_CONFIG_HOME, default to ~/.config
        let config_dir = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs::home_dir().unwrap().join(".config"));
        let path = config_dir.join("agents/AGENTS.md");
        self.load_cached(&path)
    }

    fn load_project(&self) -> Option<String> {
        for name in ["AGENTS.md", "CLAUDE.md"] {
            let path = self.project_path.join(name);
            if let Some(content) = self.load_cached(&path) {
                return Some(content);
            }
        }
        None
    }

    fn load_cached(&self, path: &Path) -> Option<String> {
        // Check mtime, return cached if fresh, else reload
    }
}
```

#### Integration with ContextManager

```rust
impl ContextManager {
    pub fn new(
        base_prompt: String,
        instruction_loader: Arc<InstructionLoader>,
    ) -> Self { ... }

    fn render_system_prompt(&self, ...) -> String {
        let mut parts = vec![self.base_prompt.clone()];

        if let Some(global) = InstructionLoader::load_global_instructions() {
            parts.push(format!("# Global Instructions\n{}", global));
        }

        if let Some(project) = self.instruction_loader.load_project_instructions() {
            parts.push(format!("# Project Instructions\n{}", project));
        }

        // ... skill, plan

        parts.join("\n\n")
    }
}
```

### Caching Strategy

1. **File mtime check**: Before each prompt render, check if instruction file mtime changed
2. **Cache invalidation**: If mtime changed, reload file
3. **Render cache**: Existing RenderCache in ContextManager handles final prompt caching

### Token Budget Considerations

| Source    | Typical Size | Max Reasonable |
| --------- | ------------ | -------------- |
| Base      | 50           | 100            |
| Global    | 500          | 2,000          |
| Project   | 1,000        | 5,000          |
| Skill     | 500          | 2,000          |
| Plan      | 200          | 500            |
| **Total** | **2,250**    | **9,600**      |

With 200k context, 10k for instructions is acceptable (~5%).

### Edge Cases

1. **No instruction files**: Use base prompt only (current behavior)
2. **File read errors**: Log warning, continue without that layer
3. **Very large files**: Truncate with warning (> 10k tokens)
4. **Binary/invalid UTF-8**: Skip with warning
5. **Symlinks**: Follow symlinks (allow shared configs)

### Future Enhancements

1. **Watch mode**: inotify/FSEvents for live reload
2. **Include directives**: `<!-- include: ./other.md -->`
3. **Conditional sections**: `<!-- if: provider == "anthropic" -->`
4. **Template variables**: `{{ cwd }}`, `{{ model }}`

## Implementation Plan

1. Create `src/agent/instructions.rs` with InstructionLoader
2. Update ContextManager to accept InstructionLoader
3. Update Agent::new to create InstructionLoader with cwd
4. Add tests for file resolution and caching
5. Update AGENTS.md docs with supported format

## Open Questions

1. ~~Should we support `.ion/` subdirectory?~~ → Yes, `~/.ion/` for ion-specific config
2. ~~Should global instructions be opt-in?~~ → No, auto-load if files exist
3. ~~How to handle conflicts?~~ → Additive layers, later wins for conflicts
4. Should we also check `~/.config/agents/rules/` or other subdirs per the standard?
5. Cross-platform: test Windows `%APPDATA%\agents\` path handling
