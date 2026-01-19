# Rust File Finder Crates Research

## Summary

**Recommendation: Use the `ignore` crate** - it provides fd-like functionality as a library, respects .gitignore, and is battle-tested (90M+ downloads, same author as ripgrep).

## Key Findings

### 1. fd is Binary-Only (Not a Library)

fd-find is published as a **binary crate only**. From GitHub issue #837:

> "As it currently exists not really. The Cargo.toml only defines a [bin] section and everything in the src/ directory exists within that binary."

The fd maintainer (sharkdp) confirmed he won't split it into lib+cli due to maintenance burden:

> "The main problem is that it causes much more maintenance. Because now the interface between the library and the CLI frontend becomes a public API that needs to be carefully designed."

**fd's recommendation**: Use the `ignore` crate (which fd depends on).

### 2. Crate Comparison

| Crate      | Downloads | Author     | Purpose               | .gitignore       | Parallel |
| ---------- | --------- | ---------- | --------------------- | ---------------- | -------- |
| `ignore`   | 91M       | BurntSushi | Full fd-like walker   | Yes              | Yes      |
| `walkdir`  | 324M      | BurntSushi | Basic recursive walk  | No               | No       |
| `globset`  | 129M      | BurntSushi | Glob pattern matching | No               | N/A      |
| `globwalk` | -         | -          | Glob + walk combined  | Partial          | No       |
| `wax`      | -         | -          | Advanced glob walk    | Yes (via filter) | No       |

### 3. The `ignore` Crate (Recommended)

From the ripgrep repository. Provides:

- Recursive directory traversal
- Respects `.gitignore`, `.ignore`, `.git/info/exclude`
- Parallel walking (via `WalkParallel`)
- File type filtering
- Max depth control
- Hidden file filtering
- Custom ignore patterns

**Dependencies**:

```
crossbeam-deque, globset, log, memchr, regex-automata, same-file, walkdir
```

**Basic Usage**:

```rust
use ignore::Walk;

for result in Walk::new("./") {
    match result {
        Ok(entry) => println!("{}", entry.path().display()),
        Err(err) => println!("ERROR: {}", err),
    }
}
```

**Advanced Usage (WalkBuilder)**:

```rust
use ignore::WalkBuilder;

let walker = WalkBuilder::new("./")
    .hidden(false)          // Include hidden files
    .ignore(true)           // Respect .ignore files
    .git_ignore(true)       // Respect .gitignore files
    .git_global(true)       // Respect global gitignore
    .git_exclude(true)      // Respect .git/info/exclude
    .max_depth(Some(5))     // Limit depth
    .follow_links(false)    // Don't follow symlinks
    .add_custom_ignore_filename(".myignore")  // Custom ignore file
    .build();

for entry in walker {
    // ...
}
```

**Parallel Walking**:

```rust
use ignore::WalkBuilder;

WalkBuilder::new("./")
    .build_parallel()
    .run(|| {
        Box::new(|entry| {
            if let Ok(entry) = entry {
                println!("{}", entry.path().display());
            }
            ignore::WalkState::Continue
        })
    });
```

### 4. When to Use Each Crate

| Use Case                                           | Crate               |
| -------------------------------------------------- | ------------------- |
| Full fd-like behavior (gitignore, types, parallel) | `ignore`            |
| Simple directory walk, no filtering                | `walkdir`           |
| Just need glob pattern matching                    | `globset`           |
| Glob patterns + walk in one                        | `globwalk` or `wax` |

### 5. File Type Filtering with `ignore`

The `ignore` crate includes file type definitions (same as ripgrep):

```rust
use ignore::types::TypesBuilder;
use ignore::WalkBuilder;

let mut types = TypesBuilder::new();
types.add_defaults();  // Built-in types (rust, js, py, etc.)
types.select("rust");  // Only Rust files

let walker = WalkBuilder::new("./")
    .types(types.build().unwrap())
    .build();
```

### 6. Integration with globset

For custom glob filtering alongside ignore:

```rust
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;

let glob = Glob::new("**/*.rs")?.compile_matcher();

for entry in WalkBuilder::new("./").build() {
    if let Ok(entry) = entry {
        if glob.is_match(entry.path()) {
            // Process Rust files
        }
    }
}
```

## Recommendation for ion

Use `ignore` as the primary crate for the list/find tool:

```toml
[dependencies]
ignore = "0.4"
globset = "0.4"  # For additional glob patterns if needed
```

**Benefits**:

- Battle-tested (used by ripgrep, fd, and many others)
- Same features as fd without shelling out
- Parallel walking for large directories
- Respects all standard ignore files
- File type filtering built-in
- Well-maintained by BurntSushi

**Implementation Notes**:

- Use `WalkBuilder` for configurable walks
- Use `WalkParallel` for large directory scans
- Combine with `globset` for pattern matching beyond .gitignore rules

## Sources

- https://crates.io/crates/ignore (91M downloads)
- https://crates.io/crates/walkdir (324M downloads)
- https://crates.io/crates/globset (129M downloads)
- https://github.com/sharkdp/fd/issues/837 (fd library discussion)
- https://docs.rs/ignore/latest/ignore/struct.WalkBuilder.html
