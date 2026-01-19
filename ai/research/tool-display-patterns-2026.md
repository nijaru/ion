# Tool Display Patterns in Coding Agent TUIs (2026)

**Research Date**: 2026-01-19
**Purpose**: Document how leading coding agents display tool calls and results in their TUIs
**Scope**: Visual patterns, collapsed/expanded states, rendering approaches

---

## Summary

| Agent        | Framework     | Collapse/Expand | Status Icons   | Result Truncation | Syntax Highlighting |
| ------------ | ------------- | --------------- | -------------- | ----------------- | ------------------- |
| **Crush**    | BubbleTea/Go  | No (inline)     | Yes (3 states) | 10 lines + "..."  | Yes (file-based)    |
| **Pi**       | Custom TS TUI | Yes (Ctrl+O)    | Via theme      | Configurable      | Yes (theme-based)   |
| **OpenCode** | BubbleTea/Go  | Yes             | Yes            | Yes               | Yes (LSP-enhanced)  |
| **Codex**    | Ink (React)   | Yes (Ctrl+R)    | Yes            | Yes               | Yes                 |
| **Amp**      | Custom TS     | Space to expand | Thread-based   | Thread summary    | Yes                 |
| **Droid**    | Go TUI        | Yes             | Yes            | Yes               | Yes                 |

---

## 1. Crush (Charmbracelet)

**Repository**: https://github.com/charmbracelet/crush
**Framework**: Go + BubbleTea + Lip Gloss

### Tool Call Header Format

```
[StatusIcon] ToolName mainParam (key1=value1, key2=value2)
```

Example:

```
[*] Bash git status
[*] Edit src/main.rs (edits=3)
[*] View README.md (offset=0, limit=100)
```

### Status Icons

| State     | Icon          | Color      | Condition                             |
| --------- | ------------- | ---------- | ------------------------------------- |
| Pending   | `ToolPending` | Green dark | `result.ToolCallID == ""`             |
| Success   | `ToolSuccess` | Green      | `ToolCallID != "" && !result.IsError` |
| Error     | `ToolError`   | Red dark   | `result.IsError`                      |
| Cancelled | `ToolPending` | Muted      | `cancelled == true`                   |

### Tool-Specific Renderers

**Bash Tool**:

- Command sanitization (newlines replaced with spaces)
- Background job indicator: `PID {ShellID}` with description
- Full command and output in body

**Edit Tool**:

- Diff visualization with automatic mode selection:
  - **Split mode**: Width > 120 chars (side-by-side before/after)
  - **Unified mode**: Width <= 120 chars (traditional diff)
- Truncation at 10 lines with "... (N lines)" message

**View Tool**:

- Syntax highlighting based on file extension
- Line numbers: `%4d | lineContent` with subtle foreground
- Truncation to `responseContextHeight` (10 lines)

**Agent Tool** (nested tasks):

- Tree structure using `lipgloss/tree`
- Connectors: `---` for intermediate, `---` for last child
- Left padding of 2 spaces
- Recursive rendering of nested tool calls

### Animation

- Spinner with "Working" label during execution
- Size: 15 for main, 10 for nested (compact)

### Code Example (renderer.go)

```go
func (br baseRenderer) makeHeader(v *toolCallCmp, tool string, width int, params ...string) string {
    t := styles.CurrentTheme()
    icon := t.S().Base.Foreground(t.GreenDark).Render(styles.ToolPending)
    if v.result.ToolCallID != "" {
        if v.result.IsError {
            icon = t.S().Base.Foreground(t.RedDark).Render(styles.ToolError)
        } else {
            icon = t.S().Base.Foreground(t.Green).Render(styles.ToolSuccess)
        }
    } else if v.cancelled {
        icon = t.S().Muted.Render(styles.ToolPending)
    }
    tool = t.S().Base.Foreground(t.Blue).Render(tool)
    prefix := fmt.Sprintf("%s %s ", icon, tool)
    return prefix + renderParamList(false, width-lipgloss.Width(prefix), params...)
}
```

---

## 2. Pi (badlogic/pi-mono)

**Repository**: https://github.com/badlogic/pi-mono
**Framework**: TypeScript + Custom TUI (`@mariozechner/pi-tui`)

### Key Features

- **Ctrl+O**: Toggle tool output expansion (collapsed by default)
- **Ctrl+T**: Toggle thinking block visibility
- Differential rendering for efficiency

### Theme Colors for Tools

| Category    | Colors                                                   |
| ----------- | -------------------------------------------------------- |
| Tool titles | `toolTitle`, `toolOutput`                                |
| Diffs       | `toolDiffAdded`, `toolDiffRemoved`, `toolDiffContext`    |
| Backgrounds | `toolPendingBg`, `toolSuccessBg`, `toolErrorBg`          |
| Syntax      | `syntaxComment`, `syntaxKeyword`, `syntaxFunction`, etc. |

### Custom Tool Rendering

Extensions can provide custom rendering via `renderCall` and `renderResult`:

```typescript
// From examples/extensions/todo.ts
export const extension = {
  tools: [
    {
      name: "todo_list",
      description: "Manage todo items",
      renderCall: (call, theme) => {
        return theme.fg("toolTitle", "Todo: ") + call.params.action;
      },
      renderResult: (result, theme) => {
        return theme.fg("toolOutput", JSON.stringify(result));
      },
    },
  ],
};
```

### Component Interface

```typescript
interface Component {
  render(width: number): string[]; // One string per line
  handleInput?(data: string): void; // Keyboard input when focused
  invalidate?(): void; // Clear cached render state
}
```

---

## 3. OpenCode

**Repository**: https://github.com/anomalyco/opencode (active), https://github.com/opencode-ai/opencode (archived)
**Framework**: Go + BubbleTea (TUI) + TypeScript (backend)

### Key Features

- LSP integration for enhanced code intelligence
- Permission dialogs for tool execution
- File change tracking and visualization
- Multi-session parallel execution

### Tool Display

- Built on Charm ecosystem (same as Crush)
- Session-based organization
- Permission system with approval workflow

### Keyboard Shortcuts

| Shortcut | Action                   |
| -------- | ------------------------ |
| Ctrl+N   | Create new session       |
| Ctrl+X   | Cancel current operation |
| Ctrl+O   | Toggle model selection   |
| i        | Focus editor             |
| Esc      | Exit writing mode        |

---

## 4. OpenAI Codex CLI

**Repository**: https://github.com/openai/codex
**Framework**: TypeScript + Ink (React for terminal)

### Architecture

- Interactive terminal UI built with `ink` and `react`
- Entry point: `src/cli.tsx` (meow for arg parsing)
- Main component: `TerminalChat` via `src/app.tsx`
- Tool components: `src/components/chat/`

### Tool Display Features

- **Ctrl+R**: Expand/collapse results
- Approve/reject steps inline
- Watch agent explain plan before changes
- Transcript view with tool calls visible

### Approval Modes

| Mode        | Behavior                                        |
| ----------- | ----------------------------------------------- |
| Auto        | Read, edit, run in working dir; ask for outside |
| Read-only   | Browse only; no changes without approval        |
| Full Access | Work across machine including network           |

### Output Modes

- Interactive: Full TUI with real-time updates
- Non-interactive: `codex exec --json` for scripting
- Review mode: `/review` for dedicated code review

---

## 5. Amp (Sourcegraph)

**Repository**: https://github.com/sourcegraph/amp (CLI)
**Framework**: TypeScript (port from Zig TUI)

### Thread-Based Interface

- Each interaction is a "thread"
- Threads can be expanded to show tool results
- Shared by default for team reuse

### Key Navigation

| Key           | Action                      |
| ------------- | --------------------------- |
| j/k or arrows | Move between threads        |
| Space         | Expand thread (show result) |
| Enter         | Open thread                 |
| e             | Archive/unarchive thread    |
| Esc           | Toggle focus                |

### Tool Call Display

- Tool calls appear in transcript
- `web_search` items visible when search is enabled
- Subagents (Oracle, Librarian) have dedicated thread types

---

## 6. Factory Droid

**Repository**: Proprietary (https://docs.factory.ai)
**Framework**: Go TUI

### Display Features

- Approve/reject proposed modifications inline
- Shift+Tab to switch modes (Spec mode for planning)
- Review workflow before execution

### Key Shortcuts

| Action         | How              |
| -------------- | ---------------- |
| Send message   | Type + Enter     |
| Multi-line     | Shift+Enter      |
| Approve        | Accept in TUI    |
| Reject         | Reject in TUI    |
| Switch modes   | Shift+Tab        |
| View shortcuts | ?                |
| Exit           | Ctrl+C or `exit` |

### Slash Commands for Tools

- `/review` - AI-powered code review
- `/settings` - Configure behavior
- `/model` - Switch models mid-session
- `/mcp` - Manage MCP servers

---

## Design Patterns Summary

### 1. Header + Body Pattern (Most Common)

```
[Icon] ToolName param1 (key=value)

  <indented body content>
  <truncated if > N lines>
  ... (X more lines)
```

### 2. Status Icon Convention

| Symbol      | Meaning   |
| ----------- | --------- |
| Spinner     | Running   |
| Checkmark   | Success   |
| X or Circle | Error     |
| Dimmed      | Cancelled |

### 3. Result Truncation

- Default: 10 lines visible
- Show count of hidden lines
- Expand toggle (Ctrl+O, Ctrl+R, Space, etc.)

### 4. Width-Aware Rendering

- Adapt display based on terminal width
- Split vs unified diff at ~120 char threshold
- Truncate long parameters with ellipsis

### 5. Syntax Highlighting

- File extension detection for language
- Theme-based color schemes
- Line numbers for code context

### 6. Nested Tool Calls

- Tree structure with connectors
- Recursive rendering
- Smaller spinners for nested items

---

## Recommendations for Ion

### Adopt

1. **Status icon pattern**: Green/red/muted for pending/success/error
2. **Header format**: `[Icon] ToolName mainParam (key=value, ...)`
3. **10-line truncation**: Show count of hidden lines
4. **Width-aware diffs**: Split above 120 chars
5. **Ctrl+O toggle**: Expand/collapse tool output

### Consider

1. **Tree view for nested tools**: Like Crush's agent renderer
2. **Syntax highlighting**: Use syntect or tree-sitter
3. **Spinner animation**: "Working" label during execution
4. **Permission flow**: Inline approve/reject like Droid

### Implementation in Rust

```rust
// Example tool header rendering
fn render_tool_header(
    name: &str,
    params: &[(&str, &str)],
    status: ToolStatus,
    width: usize,
) -> String {
    let icon = match status {
        ToolStatus::Pending => style("*").green().dim(),
        ToolStatus::Success => style("*").green(),
        ToolStatus::Error => style("!").red().dim(),
        ToolStatus::Cancelled => style("*").dim(),
    };

    let tool = style(name).blue();
    let param_str = render_params(params, width - name.len() - 4);

    format!("{} {} {}", icon, tool, param_str)
}
```

---

## References

- Crush source: https://github.com/charmbracelet/crush/blob/main/internal/tui/components/chat/messages/renderer.go
- Pi TUI docs: https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/tui.md
- OpenCode: https://opencode.ai/docs/
- Codex CLI: https://developers.openai.com/codex/cli/features/
- Amp manual: https://ampcode.com/manual
- Droid quickstart: https://docs.factory.ai/cli/getting-started/quickstart
- DeepWiki (Crush): https://deepwiki.com/charmbracelet/crush/5.3-message-and-tool-rendering
