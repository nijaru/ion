# Agent Scaffolding Research

**Researched**: 2025-10-27

## Key Finding: Simple Scaffolding >> Complex Orchestration

### SWE-Agent (May 2024)

**Result**: 3-5x improvement from interface design alone

**What worked**:
- **Custom search commands**: Limit output to 50 results max
- **Windowed file viewer**: Show 100 lines at a time (not whole files)
- **Built-in linting**: Reject syntax errors immediately
- **Context management**: Keep last 5 steps, collapse older ones

**What DIDN'T work**:
- ❌ Multi-agent orchestration
- ❌ Complex reasoning systems
- ❌ External planning phases

**Numbers**:
```
Simple agent with good interface: 12.47% pass@1
Previous RAG systems: 3.8% pass@1
= 3-5x improvement
```

### SWE-Bench Pro (Sept 2024)

**Finding**: Frontier models drop dramatically on unseen code

**Performance**:
```
SWE-Bench Verified (seen in training): 70%+
SWE-Bench Pro (unseen code): 23%

GPT-5: 23.3%
Claude Opus 4.1: 23.1%
```

**Implication**: Gap isn't closing from model improvements alone. Agent scaffolding matters more.

### Application to Aircher

**Problems with our current tools**:
1. ❌ read_file returns entire files (should window to 100 lines max)
2. ❌ No linting/validation in edit_file (should auto-reject syntax errors)
3. ❌ No result limits in search (should max 50 results)
4. ❌ No context management (should keep last 5 interactions)

**Week 2 Fixes**:
- Add windowing to read_file (max 100 lines, show hidden counts)
- Add tree-sitter validation to edit_file (auto-reject syntax errors)
- Limit search_code results (max 50, ranked by relevance)
- Implement context management (last 5 interactions, collapse older)

## LM-Centric Interface Patterns

### Windowed File Viewer (CRITICAL)

**Pattern**:
```rust
pub struct FileView {
    line_start: usize,     // Current window position
    line_end: usize,       // End of window
    total_lines: usize,    // Total file size
    window_size: usize,    // Typically 100
}

// Output format:
{
    "content": "[100 lines max]",
    "line_start": 0,
    "line_end": 100,
    "hidden_above": 0,
    "hidden_below": 5000,
    "commands": ["scroll_up", "scroll_down", "goto_line"]
}
```

**Why**: Models get overwhelmed with too much context. 100-line windows are optimal.

### Error Guardrails (CRITICAL)

**Pattern**:
```rust
async fn edit_file(path: &Path, new_content: &str) -> Result<EditResult> {
    // 1. Apply edit
    let backup = create_backup(path).await?;
    fs::write(path, new_content).await?;

    // 2. VALIDATE (tree-sitter syntax check)
    let is_valid = validate_syntax(path).await?;

    // 3. Auto-reject if invalid
    if !is_valid {
        restore_backup(path, &backup).await?;
        return Ok(EditResult {
            success: false,
            error: "Syntax error - edit rejected. Fix and try again."
        });
    }

    Ok(EditResult { success: true })
}
```

**Why**: One syntax error compounds into many failed attempts. Linting prevents this.

### Result Limits (CRITICAL)

**Pattern**:
```rust
pub struct SearchResult {
    results: Vec<Match>,    // Max 50
    total_found: usize,     // Actual count
    truncated: bool,        // If more than 50 exist
}
```

**Why**: Models can't process 500 search results effectively. 50 is the limit.

### Context Management (IMPORTANT)

**Pattern**:
```
Prompt = System + Last 5 interactions + Current task

Interaction format:
[Thought] "I should check the error logs"
[Action] search_file("error.log", "exception")
[Observation] Found 3 matches (lines 45, 67, 89)

Keep last 5, collapse older into summary
```

**Why**: Prevents context window overflow while maintaining continuity.

## Expected Impact

**Conservative estimate**:
```
Current (no guardrails): ~20% success on complex tasks

+ LM-centric interfaces: +2x = 40%
+ Error guardrails: +1.5x = 60%
+ Memory (Week 2-3): +1.3x = 78%

Realistic: 60-70% success rate
```

## Implementation Priority

**Week 2 (High Priority)**:
1. Windowed file viewer (read_file)
2. Linting/validation (edit_file)
3. Result limits (search_code)

**Week 3 (Medium Priority)**:
4. Context management (conversation history)
5. Scroll commands (file navigation)

**Week 4+ (Lower Priority)**:
6. Advanced features (diff preview, etc.)

## Sources

- SWE-Agent paper (May 2024): https://arxiv.org/abs/2405.15793
- SWE-Bench Pro (Sept 2024): https://scale.com/blog/swe-bench-pro
- Emergent Mind summary: https://www.emergentmind.com/topics/swe-agent-scaffold
