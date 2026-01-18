# Design: Diff Highlighting for Edits

**Task:** tk-smqs
**Status:** Open
**Priority:** UX Polish

## Overview

Display file edits with syntax-highlighted diffs like Claude Code. Show additions in green, deletions in red, with specific changed text highlighted brighter.

## Visual Design

```
⏺ Edit(src/config.rs)
  ⎿ Added 2 lines, removed 1 line
    45      pub model: Option<String>,
    46      pub data_dir: PathBuf,
    47 -    pub default_timeout: u64,
    47 +    pub timeout: u64,
    48 +    pub retry_count: u32,
    49      pub provider_prefs: ProviderPrefs,
```

**Color scheme:**

- Removed lines: dim red background, bright red for specific removed text
- Added lines: dim green background, bright green for specific added text
- Context lines: normal/dim
- Line numbers: dim gray

## Implementation

### Dependencies

Already have:

- `similar` crate (in Cargo.toml) - for diff computation
- `ansi-to-tui` - for ANSI parsing (could use for consistent styling)

### Data Flow

1. **Edit tool** returns `ToolResult` with old/new content
2. **Agent event** carries diff info to TUI
3. **MessageList** stores diff data
4. **Render** generates styled Lines

### Changes Required

#### 1. ToolResult Enhancement

```rust
// src/tool/mod.rs
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    pub metadata: Option<serde_json::Value>,
    pub diff: Option<DiffInfo>,  // NEW
}

pub struct DiffInfo {
    pub file_path: String,
    pub old_content: String,
    pub new_content: String,
    pub lines_added: usize,
    pub lines_removed: usize,
}
```

#### 2. Edit Tool Update

```rust
// src/tool/builtin/edit.rs
// After successful edit, populate diff info
Ok(ToolResult {
    content: format!("Edited {}", path),
    is_error: false,
    metadata: Some(json!({ "path": path })),
    diff: Some(DiffInfo {
        file_path: path.to_string(),
        old_content: old.clone(),
        new_content: new.clone(),
        lines_added: count_added,
        lines_removed: count_removed,
    }),
})
```

#### 3. AgentEvent Update

```rust
// src/agent/mod.rs
pub enum AgentEvent {
    // ...existing...
    ToolCallResult(String, String, bool, Option<DiffInfo>),  // Add diff
}
```

#### 4. Diff Rendering

```rust
// src/tui/diff.rs (new file)
use similar::{ChangeTag, TextDiff};
use ratatui::prelude::*;

pub fn render_diff(diff: &DiffInfo) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Header
    lines.push(Line::from(vec![
        Span::styled("  ⎿ ", Style::default().dim()),
        Span::styled(
            format!("Added {} lines, removed {} lines",
                diff.lines_added, diff.lines_removed),
            Style::default().dim().italic(),
        ),
    ]));

    // Compute diff
    let text_diff = TextDiff::from_lines(&diff.old_content, &diff.new_content);

    for change in text_diff.iter_all_changes() {
        let (prefix, style) = match change.tag() {
            ChangeTag::Delete => ("-", Style::default().fg(Color::Red).dim()),
            ChangeTag::Insert => ("+", Style::default().fg(Color::Green)),
            ChangeTag::Equal => (" ", Style::default().dim()),
        };

        let line_num = change.old_index()
            .or(change.new_index())
            .map(|n| format!("{:4}", n + 1))
            .unwrap_or_else(|| "    ".to_string());

        lines.push(Line::from(vec![
            Span::styled(format!("    {} ", line_num), Style::default().dim()),
            Span::styled(prefix, style),
            Span::styled(format!(" {}", change.value().trim_end()), style),
        ]));
    }

    lines
}
```

#### 5. Integration in TUI

```rust
// src/tui/mod.rs - in Sender::Tool rendering
if let Some(diff) = &entry.diff {
    let diff_lines = diff::render_diff(diff);
    chat_lines.extend(diff_lines);
} else {
    // existing markdown rendering
}
```

## Word-Level Highlighting (Phase 2)

For highlighting specific changed words within a line:

```rust
use similar::{Algorithm, TextDiff};

// Compare individual lines character-by-character
let line_diff = TextDiff::configure()
    .algorithm(Algorithm::Myers)
    .diff_chars(old_line, new_line);

// Build spans with brighter colors for changed chars
```

## Testing

1. Unit test diff computation with known inputs
2. Snapshot test rendered output
3. Manual test with real edits (rust, python, json files)

## Open Questions

1. Show full file or just changed region with context?
2. Max lines before truncating diff display?
3. Syntax highlighting on top of diff colors? (complex)
