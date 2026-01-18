# Next Session Context

## Immediate Priority: Dependency Upgrades

Completed a full audit of Cargo.toml. Found SOTA replacements and unused deps. Ready to implement.

### Task Order

1. **tk-ykpu: Upgrade grep tool** (HIGH)
   - File: `src/tool/builtin/grep.rs`
   - Current: `regex` crate + manual async recursion, hardcoded ignores
   - Replace with: `ignore` crate (ALREADY IN CARGO.TOML, just unused!)
   - Benefits: .gitignore support, parallel walking, binary detection

2. **tk-cfmz: Upgrade glob tool** (HIGH)
   - File: `src/tool/builtin/glob.rs`
   - Current: `glob` crate (~400ns)
   - Replace with: `globset` via ignore (~103ns pre-compiled)

3. **tk-ha1x: Remove unused deps** (CLEANUP)
   - Remove from Cargo.toml: `walkdir`, `serde_yaml`, `glob`
   - Verify no src/ imports first

4. **tk-9tkf: Replace tiktoken-rs** (MEDIUM)
   - File: `src/compaction/counter.rs`
   - Current: `tiktoken-rs`
   - Replace with: `bpe` from github/rust-gems (4x faster)
   - Needs: API research for exact usage

### Design Doc

Full details in `ai/design/dependency-upgrades.md`:

- Research sources
- Code examples (before/after)
- What to keep vs replace

## Key Files

| File                        | Purpose                        |
| --------------------------- | ------------------------------ |
| `src/tool/builtin/grep.rs`  | Grep tool - upgrade to ignore  |
| `src/tool/builtin/glob.rs`  | Glob tool - upgrade to globset |
| `src/compaction/counter.rs` | Token counting - swap tiktoken |
| `Cargo.toml`                | Dependencies to modify         |

## Current Cargo.toml State

```toml
# ALREADY HAVE (use it!)
ignore = "0.4.25"

# REMOVE
walkdir = "2.5.0"       # Redundant
serde_yaml = "0.9.34"   # Deprecated, unused in src/
glob = "0.3.3"          # Replace with globset

# REPLACE
tiktoken-rs = "0.9.1"   # → bpe (github/rust-gems)
```

## Implementation Notes

### Grep Tool Upgrade

```rust
// Current (grep.rs:77-119) - manual recursion
impl GrepTool {
    async fn search_recursive(...) {
        // Manual dir walking, hardcoded ignores
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
    }
}

// New approach with ignore crate
use ignore::WalkBuilder;

let walker = WalkBuilder::new(path)
    .hidden(true)      // Skip hidden
    .git_ignore(true)  // Respect .gitignore
    .build();

for entry in walker {
    // entry is already filtered
}
```

### Glob Tool Upgrade

```rust
// Current (glob.rs) - uses glob crate
use glob::glob;

// New approach with globset
use ignore::overrides::OverrideBuilder;
// or use globset directly
use globset::{Glob, GlobSetBuilder};
```

## Other Open Tasks (lower priority)

| ID      | Category | Task                                           |
| ------- | -------- | ---------------------------------------------- |
| tk-3jba | BUG      | Ctrl+C not interruptible during tool execution |
| tk-smqs | IDEA     | Diff highlighting for edits                    |
| tk-otmx | UX       | Ctrl+G opens input in external editor          |
| tk-whde | UX       | Git diff stats in status line                  |
| tk-arh6 | UX       | Tool execution not visually obvious            |
| tk-o4uo | UX       | Modal escape handling                          |
| tk-iegz | IDEA     | OpenRouter provider routing modal              |

## Project State

- Phase: 5 - Polish & UX
- Status: Runnable
- Tests: 51 passing
- Branch: main (clean)

## Commands

```bash
cargo build              # Debug build
cargo test               # Run tests
tk ls                    # See all tasks
tk start <id>            # Start a task
tk done <id>             # Complete a task
```

## Start Here

```bash
# 1. Read the design doc
cat ai/design/dependency-upgrades.md

# 2. Start first task
tk start tk-ykpu

# 3. Read current grep implementation
# src/tool/builtin/grep.rs

# 4. Implement with ignore crate
```
