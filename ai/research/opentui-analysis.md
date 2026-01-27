# OpenTUI Analysis

SST's terminal UI framework. TypeScript API with Zig rendering core via FFI.

**Repository:** [github.com/sst/opentui](https://github.com/sst/opentui)
**Status:** Active development, not production-ready
**Used by:** opencode, terminaldotshop

---

## Architecture Overview

| Layer      | Implementation    | Responsibility                        |
| ---------- | ----------------- | ------------------------------------- |
| TypeScript | @opentui/core     | Component tree, layout, lifecycle     |
| FFI        | Bun.dlopen()      | Type marshaling, ~100+ functions      |
| Zig        | renderer.zig, etc | Frame diffing, ANSI gen, text buffers |
| Layout     | Yoga              | Flexbox calculations                  |

**Packages:**

- `@opentui/core` - Standalone imperative API
- `@opentui/solid` - SolidJS reconciler
- `@opentui/react` - React reconciler
- Go bindings available

---

## 1. Viewport/Rendering Model

**Double-buffered cell array system:**

```typescript
// TypeScript API
const renderer = await createCliRenderer({
  exitOnCtrlC: true,
  targetFps: 60,
  useAlternateScreen: true,
});
```

```go
// Go bindings show internals
type Cell struct {
    Char       rune
    Foreground RGBA
    Background RGBA
    Attributes uint8
}

// Dual buffer access
nextBuffer, _ := renderer.GetNextBuffer()   // Build next frame
currentBuffer, _ := renderer.GetCurrentBuffer() // Currently displayed
renderer.Render(false)  // Incremental diff
renderer.Render(true)   // Force full redraw
```

**Three-pass rendering cycle:**

1. Pre-render: Layout calculation (Yoga flexbox)
2. Render: Components draw to cell buffer
3. Post-render: Diff, ANSI generation, buffer swap

**Render modes:**

- Auto mode (default): Re-renders on tree/layout changes
- Live mode: Continuous rendering at `targetFps` via `renderer.start()`

---

## 2. Frame Diffing in Zig

Implemented in `renderer.zig`:

**Algorithm:**

1. Compare cell arrays between `currentBuffer` and `nextBuffer`
2. Identify changed regions (cell-by-cell comparison)
3. Generate ANSI sequences only for modified cells
4. Use run-length encoding for adjacent cells with identical styling
5. Batch writes to stdout

**Performance claims:** Sub-millisecond frame times, 60+ FPS for complex UIs

**Go API example:**

```go
// Render(false) = incremental/diff update
// Render(true) = force complete redraw
renderer.Render(false)
```

**ANSI optimization in `ansi.zig`:**

- Run-length encoding for repeated styles
- Escape sequence grouping
- Minimized stdout writes via batching

---

## 3. Inline Mode Support

**Yes, OpenTUI supports inline mode:**

```go
// Go bindings show the API
renderer.SetupTerminal(useAlternateScreen bool) error
renderer.CloseWithOptions(useAlternateScreen bool, splitHeight uint32) error
```

```typescript
// TypeScript
const renderer = await createCliRenderer({
  useAlternateScreen: false, // Inline mode
});
```

**Inline mode behavior:**

- `useAlternateScreen: false` renders in normal terminal buffer
- Content appears in scrollback
- `splitHeight` parameter enables hybrid cleanup on close

**Viewport offset for scrollback:**

```go
// Control rendering position within inline buffer
renderer.SetRenderOffset(offset uint32) error
```

**Limitation discovered:** No explicit documentation on maintaining scrollback history during inline rendering. The focus is on fullscreen alternate-screen TUIs.

---

## 4. Dynamic Content Height

**TextBuffer with virtual line calculation:**

```go
textBuffer := opentui.NewTextBuffer(1024, opentui.WidthMethodUnicode)
textBuffer.WriteString("Content here")
textBuffer.FinalizeLineInfo()

// Get dynamic line info for height calculation
lines, _ := textBuffer.GetLineInfo()
lineCount, _ := textBuffer.LineCount()

type LineInfo struct {
    StartIndex uint32  // Character position
    Width      uint32  // Rendered width
}
```

**Yoga layout integration:**

- Flexbox-based positioning
- Components re-render on tree/layout changes
- Height adjusts based on enabled features

**Known issue (critical):** O(n) complexity bug in `textBufferViewMeasureForDimensions`:

- Recalculates virtual lines for ALL content on every update
- During streaming: 100%+ CPU, continuous mmap allocations
- Root cause: Renders entire buffer, not just viewport

From [GitHub issue #6172](https://github.com/anomalyco/opencode/issues/6172):

> "89.8% of samples in textBufferViewMeasureForDimensions call stack"
> Rope traversal + segment callbacks lack viewport-aware optimization

---

## 5. TypeScript-Zig FFI Architecture

**Bridge mechanism:** Bun's `dlopen()` loads platform-specific binaries

**Platform binaries:**

- darwin-arm64, darwin-x64
- linux-arm64, linux-x64
- win32-arm64, win32-x64

**FFI boundary responsibilities:**

| TypeScript           | Zig                               |
| -------------------- | --------------------------------- |
| Component tree       | Cell buffer operations            |
| Layout orchestration | Frame diffing                     |
| Event handling       | ANSI generation                   |
| Lifecycle management | Text buffer (rope data structure) |
|                      | Hit grid (mouse coord mapping)    |

**Type marshaling:** Uses `bun-ffi-structs` package

**Example flow:**

```
Request -> Loop -> Layout -> Render -> Hit Grid -> Diff -> ANSI -> Output -> Swap
```

---

## Relevance to Ion's Inline Viewport Problem

### What OpenTUI Offers

| Feature                  | Status  | Notes                                  |
| ------------------------ | ------- | -------------------------------------- |
| Inline mode              | Yes     | `useAlternateScreen: false`            |
| Scrollback preservation  | Partial | `splitHeight` on close, not during use |
| Dynamic height           | Yes     | TextBuffer line tracking               |
| Efficient frame diffing  | Yes     | Cell-level comparison in Zig           |
| Viewport-aware rendering | No      | Known O(n) issue for large buffers     |

### Key Insights for Ion

1. **Dual-buffer diffing works well** - Their cell comparison + ANSI RLE achieves good performance for typical frames

2. **Inline mode exists but is secondary** - API supports it, but design optimized for alternate screen

3. **Dynamic height via line info tracking** - `LineInfo` struct with `StartIndex`/`Width` enables wrap-aware height calculation

4. **O(n) full-buffer measurement is a trap** - Their architecture suffers from measuring entire content on every update. Ion must avoid this with viewport-only rendering.

5. **Render offset for viewport control** - `SetRenderOffset()` suggests a mechanism for viewport positioning within inline buffers

### Recommendations for Ion

1. **Do NOT adopt their text buffer approach** - The O(n) recalculation pattern causes serious performance issues at scale

2. **Consider their frame diffing strategy** - Cell-level diff + ANSI RLE is sound, could inform ratatui optimizations

3. **Inline mode needs viewport awareness** - They haven't solved the inline + dynamic height + scrollback problem well

4. **Their dual-buffer pattern is standard** - Nothing novel, but confirms the approach

---

## Sources

- [SST OpenTUI GitHub](https://github.com/sst/opentui)
- [DeepWiki: OpenTUI Architecture](https://deepwiki.com/sst/opentui)
- [DeepWiki: Terminal Control](https://deepwiki.com/sst/opentui/4.2-terminal-control)
- [Go Package Documentation](https://pkg.go.dev/github.com/sst/opentui/packages/go)
- [OpenCode Issue #6172: O(n) Rendering Bug](https://github.com/anomalyco/opencode/issues/6172)
- [TypeVar: OpenTUI Installation](https://typevar.dev/articles/sst/opentui)

---

**Date:** 2026-01-26
