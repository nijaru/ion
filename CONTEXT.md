# Session Context

## Immediate Priority

### 1. Cleanup Tasks (do first)

| ID      | Task                                             |
| ------- | ------------------------------------------------ |
| tk-vpj4 | Update GitHub repo description via `gh` CLI      |
| tk-btof | Clean up README (remove redundant Rust mentions) |
| tk-x3vt | Evaluate removing nightly (omendb not in core)   |

**GitHub repo description** is outdated - mentions LangGraph which was removed.

**README** says "rust-based", "high-performance", "built in Rust" multiple times. Let it speak for itself.

**Nightly** was for omendb's `portable_simd`. Since memory is a plugin, core may not need nightly.

### 2. Dependency Upgrades (main work)

| ID      | Priority | Task                                           |
| ------- | -------- | ---------------------------------------------- |
| tk-ykpu | HIGH     | Upgrade grep tool to use ignore crate          |
| tk-cfmz | HIGH     | Upgrade glob tool to use globset               |
| tk-ha1x | CLEANUP  | Remove unused deps (walkdir, serde_yaml, glob) |
| tk-9tkf | MEDIUM   | Replace tiktoken-rs with GitHub bpe crate      |

**Design doc:** `ai/design/dependency-upgrades.md`

Key insight: `ignore` crate is ALREADY in Cargo.toml but unused. Grep/glob tools use inferior alternatives.

## Key Files

| File                        | Purpose                        |
| --------------------------- | ------------------------------ |
| `src/tool/builtin/grep.rs`  | Grep tool - upgrade to ignore  |
| `src/tool/builtin/glob.rs`  | Glob tool - upgrade to globset |
| `src/compaction/counter.rs` | Token counting - swap tiktoken |
| `Cargo.toml`                | Dependencies to modify         |
| `rust-toolchain.toml`       | Nightly config - maybe remove  |
| `README.md`                 | Clean up language              |

## Cargo.toml Changes Needed

```toml
# ALREADY HAVE (use it!)
ignore = "0.4.25"

# REMOVE
walkdir = "2.5.0"       # Redundant with ignore
serde_yaml = "0.9.34"   # Deprecated, unused in src/
glob = "0.3.3"          # Replace with globset

# REPLACE
tiktoken-rs = "0.9.1"   # → bpe (github/rust-gems, 4x faster)
```

## Implementation Notes

### Grep Tool (ignore crate)

```rust
// Current: manual recursion with hardcoded ignores
if name.starts_with('.') || name == "target" || name == "node_modules" {
    continue;
}

// New: ignore crate handles .gitignore, hidden files, binaries
use ignore::WalkBuilder;
let walker = WalkBuilder::new(path)
    .hidden(true)
    .git_ignore(true)
    .build();
```

### Glob Tool (globset)

```rust
// Current: glob crate (~400ns)
use glob::glob;

// New: globset (~103ns pre-compiled)
use globset::{Glob, GlobSetBuilder};
```

## Other Open Tasks

| ID      | Category | Task                                           |
| ------- | -------- | ---------------------------------------------- |
| tk-3jba | BUG      | Ctrl+C not interruptible during tool execution |
| tk-smqs | IDEA     | Diff highlighting for edits                    |
| tk-otmx | UX       | Ctrl+G opens input in external editor          |
| tk-whde | UX       | Git diff stats in status line                  |
| tk-arh6 | UX       | Tool execution not visually obvious            |
| tk-o4uo | UX       | Modal escape handling                          |

## Project State

- Phase: 5 - Polish & UX
- Status: Runnable
- Tests: 51 passing
- Branch: main

## Commands

```bash
cargo build              # Debug build
cargo test               # Run tests
tk ls                    # See all tasks
tk start <id>            # Start a task
tk done <id>             # Complete a task
gh repo edit -d "desc"   # Update repo description
```

## Start Sequence

```bash
# 1. Quick cleanup first
gh repo edit -d "TUI coding agent with multi-provider LLM support"
tk done tk-vpj4

# 2. Check if nightly needed
grep -r "portable_simd\|#!\[feature" src/

# 3. Then dependency upgrades
tk start tk-ykpu
```
