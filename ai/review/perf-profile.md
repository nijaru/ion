# ion Performance Profile

**Date:** 2026-01-25
**Version:** 0.0.0
**Platform:** macOS Darwin 25.2.0 (M3 Max)

## Baseline Measurements

### Startup Time

```
hyperfine --warmup 3 --runs 20 'target/release/ion --help'

Time (mean +/- sd):     4.3 ms +/- 0.4 ms
Range (min ... max):    3.5 ms ... 5.8 ms
```

**Assessment:** Excellent. Sub-5ms startup for `--help` indicates minimal initialization overhead. The `#[tokio::main]` async runtime initializes, clap parses args, and exits immediately without loading config or other resources.

### Binary Size

| Metric          | Value |
| --------------- | ----- |
| Release binary  | 32 MB |
| Stripped binary | 29 MB |

**Assessment:** Moderately large for a CLI tool, but acceptable given the feature set (TUI, multiple LLM providers, syntax highlighting, SQLite, MCP support).

### Binary Composition (cargo-bloat)

Top contributors to `.text` section (9.6 MB):

| Crate          | Size   | % of .text |
| -------------- | ------ | ---------- |
| std            | 1.8 MB | 18.8%      |
| [Unknown]      | 1.5 MB | 15.9%      |
| ion (app code) | 1.1 MB | 11.1%      |
| reqwest        | 726 KB | 7.4%       |
| minijinja      | 553 KB | 5.6%       |
| regex_automata | 426 KB | 4.4%       |
| h2             | 343 KB | 3.5%       |
| llm_connector  | 235 KB | 2.4%       |
| tokio          | 225 KB | 2.3%       |
| clap_builder   | 210 KB | 2.1%       |

**Analysis:**

- `reqwest` (HTTP client) is the largest non-std dependency - necessary for LLM API calls
- `minijinja` (templating) - used for skill/prompt templating
- `regex_automata` - used by grep tool and various parsing
- `h2` - HTTP/2 support for reqwest
- These are all core features, not obvious candidates for removal

### Build Time

| Build Type                  | Time                            |
| --------------------------- | ------------------------------- |
| Incremental (touch main.rs) | 10.2s                           |
| Full clean build            | ~50s (estimated from --timings) |

**Assessment:** Acceptable for a ~15k LOC Rust project with heavy dependencies.

### Test Suite

```
cargo test --release: 89 tests, 0.28s
```

**Assessment:** Fast test suite, no performance concerns.

### Codebase Size

```
Total: 14,932 lines of Rust code
```

Key modules by size:

- `tui/composer/mod.rs`: 1,082 lines (input handling)
- `tui/render.rs`: 800 lines (UI rendering)
- `agent/mod.rs`: 735 lines (agent loop)
- `provider/registry.rs`: 676 lines (model registry)
- `tui/model_picker.rs`: 657 lines (model selection UI)

## Code Review: Memory Patterns

### Potential Concerns

1. **MessageList unbounded growth** (`src/tui/message_list.rs:206-212`)
   - `entries: Vec<MessageEntry>` grows unbounded during long sessions
   - Each entry caches markdown rendering
   - Mitigated by session-based design (users start fresh sessions)
   - Recommendation: Consider entry limit with LRU eviction for very long sessions

2. **Token counting overhead** (`src/compaction/counter.rs:27-29`)
   - Uses `bpe-openai::cl100k_base()` for accurate token counting
   - Called on every message for context tracking
   - The tokenizer is lazily loaded and reused (good)
   - No obvious inefficiency, BPE counting is inherently O(n)

3. **Syntax highlighting lazily loaded** (`src/tui/highlight.rs:11-12`)
   - `SyntaxSet` and `ThemeSet` are `LazyLock` (good)
   - Only loaded on first code block highlight
   - syntect loads ~100 syntax definitions, but this is amortized

4. **Arc<Vec<ContentBlock>> for messages** (`src/provider/types.rs`)
   - Using `Arc` avoids cloning message content during streaming
   - Good pattern for avoiding allocation during hot path

### No Obvious Memory Leaks

- Session data persisted to SQLite, not held in memory
- Channels (`mpsc`) properly bounded with capacity 100
- Abort tokens (`CancellationToken`) properly propagated

## Optimization Opportunities

### Low Effort, Low Impact

1. **Strip binary in release** - saves 3 MB
   - Add to Cargo.toml: `[profile.release] strip = true`

2. **LTO for smaller binary** - potential 10-20% size reduction
   - `[profile.release] lto = "thin"` or `lto = true`
   - Trade-off: longer compile times

### Medium Effort, Medium Impact

1. **Lazy config loading for --help**
   - Currently `Config::load()` is not called for `--help` (already optimized)
   - No action needed

2. **Feature flags for optional providers**
   - Could gate Ollama, Groq, etc. behind features
   - Diminishing returns - HTTP client dominates binary size

### Not Recommended

1. **Replacing reqwest** - Would require significant rewrite for marginal gains
2. **Custom tokenizer** - bpe-openai is well-optimized
3. **Removing syntect** - Core feature for code display

## Summary

| Metric            | Value  | Status     |
| ----------------- | ------ | ---------- |
| Startup time      | 4.3 ms | Excellent  |
| Binary size       | 32 MB  | Acceptable |
| Test time         | 0.28s  | Excellent  |
| Incremental build | 10.2s  | Acceptable |
| Memory patterns   | Clean  | No leaks   |

**Conclusion:** The codebase is well-optimized with no critical performance issues. The binary size is driven by necessary dependencies (HTTP client, syntax highlighting, async runtime). Startup time is excellent due to lazy initialization patterns.

### Recommended Actions

1. Add `strip = true` to release profile (easy win)
2. Monitor MessageList growth in long sessions (future work)
3. Consider `lto = "thin"` if binary size becomes a concern
