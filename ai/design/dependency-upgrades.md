# Design: Dependency Upgrades

**Status:** Planned
**Priority:** High (grep/glob), Medium (tokenizer), Low (cleanup)

## Overview

Audit of current dependencies identified several opportunities for SOTA replacements and cleanup of redundant/deprecated crates.

## Current State

| Crate                      | Usage           | Issue                                  |
| -------------------------- | --------------- | -------------------------------------- |
| `regex` + manual recursion | grep tool       | Slow, no .gitignore, hardcoded ignores |
| `glob`                     | glob tool       | Basic, no filtering                    |
| `walkdir`                  | unused          | Redundant - `ignore` already in deps   |
| `tiktoken-rs`              | token counting  | Slower than alternatives               |
| `serde_yaml`               | none (in src/)  | Deprecated, unmaintained               |
| `tokenizers`               | embedding model | Keep - needed for HF tokenizer.json    |

## Planned Changes

### 1. Grep Tool → ignore + grep-searcher

**Current:** `regex` crate + manual async recursion
**Replace with:** `ignore` crate (already in Cargo.toml, unused)

Benefits:

- Respects .gitignore automatically
- Parallel directory walking
- Handles hidden files, binary detection
- Much faster on large codebases

```rust
// Before: manual recursion with hardcoded ignores
if name.starts_with('.') || name == "target" || name == "node_modules" {
    continue;
}

// After: ignore crate handles all of this
use ignore::WalkBuilder;
WalkBuilder::new(path)
    .hidden(true)      // Skip hidden files
    .git_ignore(true)  // Respect .gitignore
    .build()
```

### 2. Glob Tool → globset (via ignore)

**Current:** `glob` crate
**Replace with:** `globset` (comes with ignore, maintained by ripgrep team)

Performance (pre-compiled patterns):

- glob: ~400ns
- globset: ~103ns

### 3. Token Counter → GitHub's bpe

**Current:** `tiktoken-rs`
**Replace with:** `bpe` from [github/rust-gems](https://github.com/github/rust-gems)

Benefits:

- 4x faster than tiktoken
- Same cl100k/o200k tokenizer support
- MIT licensed, actively maintained

```rust
// Before
use tiktoken_rs::cl100k_base;
let bpe = cl100k_base()?;
let tokens = bpe.encode_with_special_tokens(text);

// After
use bpe::byte_pair_encoding::BytePairEncoding;
// API TBD - need to verify exact usage
```

### 4. Remove Unused Dependencies

```toml
# Remove from Cargo.toml
walkdir = "2.5.0"       # Redundant with ignore
serde_yaml = "0.9.34"   # Deprecated, not used in src/
glob = "0.3.3"          # Replaced by globset
```

### 5. Keep As-Is

| Crate        | Reason                                       |
| ------------ | -------------------------------------------- |
| `regex`      | Still needed for designer.rs JSON extraction |
| `chrono`     | Works fine, jiff is newer but less proven    |
| `tokenizers` | Needed for embedding model (HF format)       |
| `similar`    | Good diff library, no better alternative     |

## Implementation Order

1. **grep/glob tools** - Use `ignore` (already a dep, easy win)
2. **Remove unused deps** - walkdir, serde_yaml, glob
3. **Token counter** - Swap tiktoken-rs for bpe (needs API research)

## Research Sources

- [ripgrep libraries](https://github.com/BurntSushi/ripgrep)
- [GitHub's rust-gems BPE](https://github.blog/ai-and-ml/llms/so-many-tokens-so-little-time-introducing-a-faster-more-flexible-byte-pair-tokenizer/)
- [serde_yaml deprecation](https://users.rust-lang.org/t/serde-yaml-deprecation-alternatives/108868)
- [globset vs glob](https://github.com/rust-lang/glob/issues/59)
- [jiff datetime](https://docs.rs/jiff/latest/jiff/_documentation/comparison/index.html)
