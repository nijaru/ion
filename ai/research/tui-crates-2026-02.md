# TUI Crate Evaluation for Ion (2026-02)

**Date:** 2026-02-11
**Context:** Ion uses an inline rendering model (no alternate screen) where chat history is printed to native terminal scrollback and a bottom UI (input, progress, status) is cursor-positioned. This is fundamentally different from fullscreen retained-mode TUIs.
**Prior research:** inline-tui-patterns-2026.md, tui-hybrid-chat-libraries-2026.md, inline-tui-rendering-deep-dive-2026.md, ratatui-vs-crossterm-v3.md

---

## Answer

**No crate solves Ion's specific problem.** The inline-chat + bottom-UI pattern remains a DIY domain. The runtime-stack plan's RNK integration (tk-add8) is the only option worth spiking, but carries significant risk due to RNK's immaturity. All other crates are wrong-shaped for Ion's rendering model.

---

## Crate Evaluation Matrix

| Crate                   | Inline Mode?               | Scrollback to Terminal?         | Maturity                                  | Solves Resize/Autowrap?             | Verdict                         |
| ----------------------- | -------------------------- | ------------------------------- | ----------------------------------------- | ----------------------------------- | ------------------------------- |
| **rnk**                 | Yes (default)              | Yes (println above UI)          | Very low (158 DL, single author)          | Unknown -- no evidence              | Spike candidate with high risk  |
| **ratatui**             | Viewport::Inline (limited) | insert_before() only            | Very high (17.9k stars, active)           | No -- fixed viewport, resize broken | Wrong architecture              |
| **r3bl_tui**            | Partial TUI / REPL mode    | Partial (stdout writes in REPL) | Low-medium (8k DL/mo, single org)         | No evidence                         | Wrong shape, massive dep tree   |
| **tui-realm**           | No                         | No                              | Medium (878 stars, active)                | No                                  | Wrapper on ratatui, same limits |
| **cursive**             | No                         | No                              | Medium (24k DL/mo, last release Aug 2024) | No                                  | Fullscreen only                 |
| **termwiz**             | No explicit mode           | Surface abstraction, no hybrid  | High (3.6M total DL, wezterm)             | No -- low-level like crossterm      | Different API, no advantage     |
| **rxtui**               | No                         | No                              | Very low (0.1.8, 311 stars)               | No                                  | Fullscreen virtual DOM          |
| **crossterm** (current) | N/A -- primitive layer     | N/A -- you build it             | Very high (foundational crate)            | N/A -- bugs are in Ion's layer      | Baseline, keep                  |

---

## Detailed Evaluations

### rnk -- React-like Terminal UI (v0.17.3)

**What it is:** Ink/Bubbletea-inspired framework for Rust. Declarative UI with hooks, flexbox layout via Taffy, reconciliation-based rendering.

**Architecture:**

- 14 source modules: components, hooks, layout, renderer, reconciler, runtime, animation, cmd, core, testing
- Crossterm 0.28 backend, tokio async runtime
- Line-level diff rendering in inline mode
- `println()` API for persistent messages above the UI
- Runtime mode switching between inline and fullscreen

**Inline mode specifics:**

- Default mode; renders at current cursor position
- Output persists in terminal scrollback
- `println()` prints above the managed UI area (exactly Ion's `insert_before` pattern)
- Reconciler diffs component tree, only redraws changed lines

**Maturity concerns:**

| Metric            | Value                                                         | Assessment          |
| ----------------- | ------------------------------------------------------------- | ------------------- |
| Total downloads   | 158 (crates.io) -- but docs.rs shows v0.17.3 with 20 releases | Extremely early     |
| Weekly downloads  | Not measurable                                                | No real adoption    |
| Author            | majiayu000 (single developer)                                 | Bus factor = 1      |
| Used in           | 4 crates (2 directly)                                         | Near-zero ecosystem |
| Last release      | Feb 8, 2026 (0.17.3)                                          | Active development  |
| Breaking versions | 7 out of 20                                                   | Unstable API        |
| Documentation     | 81% documented                                                | Reasonable          |
| Test coverage     | Unknown                                                       | Risk                |
| Code size         | ~10K SLoC                                                     | Moderate            |

**Key risks:**

1. No production users at scale. Zero evidence it handles resize/autowrap correctly.
2. API breaking constantly (7 breaking changes in 20 releases).
3. Single developer -- no review, no community, no battle-testing.
4. Taffy layout engine adds complexity for what is fundamentally a 3-5 line bottom UI.
5. No evidence of synchronized output (CSI 2026) support.
6. The runtime-stack plan (tk-add8) already identifies this as a spike -- the spike should focus on whether rnk's inline renderer actually fixes real bugs vs just moving them.

**What rnk could provide (if it works):**

- Structured component model replaces ad-hoc render functions
- Flexbox layout for bottom UI avoids manual cursor math
- Diff-based rendering reduces bytes written per frame
- Hooks pattern simplifies state -> render flow

**What rnk does NOT provide:**

- Any proven solution to terminal resize/autowrap problems
- Markdown rendering (Ion would still use pulldown-cmark)
- Chat history management (still Ion's responsibility)
- Synchronized output wrapping

### ratatui -- The Ecosystem Standard (v0.30)

**Why it does not fit Ion:**

1. **Fullscreen-first design.** The `Terminal` abstraction assumes it owns the entire screen. `Viewport::Inline` is a secondary mode with known issues.

2. **Fixed viewport height.** `Viewport::Inline(8)` allocates a fixed number of lines. Ion's bottom UI height varies (input grows with multiline, progress line appears/disappears). PR #1964 (`set_viewport_height`) is still not merged.

3. **Horizontal resize is broken.** Issue #2086 and Draft PR #2355 document that horizontal resize corrupts content in inline viewport mode. This is Ion's primary pain point -- adopting ratatui would inherit the same bug.

4. **insert_before is limited.** Only inserts single lines, not multi-line blocks with layout. Issue #1426 (insert_lines_before) is open but not implemented.

5. **Inline viewport resize cannot be dynamically changed.** Must recreate the terminal to change viewport height, which disrupts rendering state.

**What ratatui does well (irrelevant to Ion):**

- Widget system, layout engine, styled text
- Massive ecosystem (878 crate dependents)
- Active maintainers, well-tested

**Could ratatui-core/ratatui-widgets be used a la carte?**
Yes -- for rendering styled blocks to a Buffer, then converting to ANSI strings. But Ion already has its own styled output through pulldown-cmark and crossterm. The marginal value is low for the dependency cost.

**Reference:** Codex CLI uses ratatui widgets for rendering but wraps a custom Terminal (not Viewport::Inline). This confirms ratatui's viewport is inadequate, but its widget rendering is useful as a subordinate component.

### r3bl_tui -- React/Elm Inspired (v0.7.6)

**Architecture:** Message-passing (Elm-style) with flexbox layout, offscreen buffer rendering, component registry. Supports three modes: Full TUI, Partial TUI (choice lists), and REPL (async readline).

**Inline mode assessment:**

- "Partial TUI" and "REPL" modes write to stdout without alternate screen
- REPL mode: async readline with spinner, can write to stdout "without clobbering the prompt"
- Sounds similar to Ion's needs, but the REPL mode is for line-at-a-time interaction, not a managed multi-line bottom UI

**Why it does not fit:**

1. The REPL mode is readline-style, not a managed viewport with progress/status/input components
2. Full TUI mode uses alternate screen
3. 56K SLoC with 37 dependencies including syntect, reqwest, chrono, sha2, mimalloc -- massive dependency footprint for marginal gain
4. 42% documented -- poor for a framework
5. No evidence of the specific "chat scrollback + positioned bottom UI" pattern

### tui-realm -- Ratatui Framework Layer (v3.3.0)

**What it is:** React/Elm component framework built ON TOP of ratatui. Adds component lifecycle, message passing, view management.

**Why it does not fit:**

- Inherits all of ratatui's limitations
- Adds more abstraction without solving the inline rendering problem
- Fullscreen assumption (ratatui alternate screen backend)
- 878 stars, reasonable maturity, but wrong architecture entirely

### cursive -- Dialog-Based TUI (v0.21.1)

**What it is:** Traditional TUI library focused on dialog/form-based interfaces. Uses layers, views, and callbacks. Multiple backends (crossterm, ncurses, termion).

**Why it does not fit:**

- Fullscreen only -- `Cursive::run()` enters event loop and takes over the terminal
- No inline mode whatsoever
- Designed for dialog-heavy apps (file pickers, forms), not streaming chat
- Last release Aug 2024 -- lower activity than other options
- 97 contributors and good maturity, but fundamentally wrong model

### termwiz -- WezTerm's Terminal Library (v0.23.3)

**What it is:** Low-level terminal manipulation library from the wezterm project. Provides Surface (2D cell buffer), terminal I/O, escape sequence parsing, and a widget system.

**Architecture:**

- `Surface`: 2D buffer of cells (character + style). Accumulates `Change` enums. Can diff against previous state.
- `BufferedTerminal`: Wraps a Terminal + Surface. Deferred rendering with optimization.
- Widget system with layout (cassowary-based)

**Why it does not fit Ion:**

1. It is a crossterm alternative, not a higher-level abstraction. Switching from crossterm to termwiz would be a rewrite of the terminal I/O layer without solving the inline rendering problem.
2. No inline/partial-screen mode. The Surface assumes ownership of a rectangular area.
3. The Surface diffing could theoretically be used for the bottom UI, but you would still need to build the scrollback insertion logic yourself.
4. High download count (3.6M) is misleading -- nearly all from wezterm itself.
5. Uses thiserror v1, lazy_static, and older dependencies (2018 edition).

**One interesting capability:** termwiz's `Terminal::buffered` could serve as a diff-rendering layer for the bottom UI area. But this is a micro-optimization that does not justify switching from crossterm.

### rxtui -- Reactive Terminal UI (v0.1.8)

**What it is:** React-inspired TUI with virtual DOM diffing, message-based state (Elm pattern), async effects. From the microsandbox/zerocore-ai project.

**Why it does not fit:**

- Fullscreen with virtual DOM -- designed for complete screen ownership
- No inline mode
- 311 GitHub stars, very early (0.1.x)
- No evidence of scrollback preservation

---

## Competitive Landscape: How Chat Agents Solve This

Every major terminal AI agent has faced the exact same problem. Their conclusions:

| Agent           | Framework                  | Inline?         | Approach                                                      | Resize Strategy             |
| --------------- | -------------------------- | --------------- | ------------------------------------------------------------- | --------------------------- |
| **Claude Code** | React + custom Ink fork    | Yes             | Differential renderer (2D buffer diff), Static for scrollback | Full clear + redraw         |
| **Gemini CLI**  | Ink (v0.15+)               | No (alt screen) | Alternate screen, print history on exit                       | Alt screen avoids it        |
| **Codex CLI**   | ratatui (widgets only)     | Yes             | Custom terminal wrapper, DECSTBM scroll regions               | Full redraw                 |
| **pi-mono**     | Custom TypeScript (pi-tui) | Yes             | Three-strategy renderer (initial/width-change/content-change) | Full clear for width change |
| **opencode**    | opentui (React/Ink-like)   | Yes             | Component-based with differential rendering                   | Full redraw                 |
| **Ion**         | Custom crossterm           | Yes             | Two-mode (row-tracking + scroll), synchronized output         | Full reprint                |

**Key observation:** Every inline agent either (a) built a custom renderer from scratch or (b) heavily forked an existing framework. None uses an off-the-shelf TUI library for the inline chat pattern. Gemini CLI is the outlier, choosing alternate screen to sidestep the problem entirely.

---

## What About Custom Abstractions Over Crossterm?

The prior research (tui-hybrid-chat-libraries-2026.md) concluded:

> "No library exists for this specific pattern. Custom abstractions over crossterm remain the correct approach."

This remains true as of 2026-02-11. What has changed:

1. **rnk exists** but is unproven. Its inline mode and println() API are architecturally aligned, but the crate is too immature for production use.
2. **Claude Code shipped a differential renderer** that reduced flickering by ~85%. The approach (2D buffer diff) is validated but was built custom on top of Ink, not extracted as a library.
3. **crossterm 0.29** added `BeginSynchronizedUpdate`/`EndSynchronizedUpdate` which Ion already uses. No new crossterm features address the inline rendering pattern.

---

## Recommendations

### 1. Continue with custom crossterm (default path)

The TUI v3 architecture plan (`ai/design/tui-v3-architecture-2026-02.md`) already defines the correct target: single render authority, deterministic frame pipeline, width-safe rendering contract. This is the proven path -- build the abstraction Ion needs rather than adapt to a library that does not fit.

**Specific improvements to pursue:**

- Differential rendering for the bottom UI (front/back buffer comparison, only write changed cells)
- Debounced resize handling (batch rapid SIGWINCH events)
- DECSTBM scroll regions (prevent bottom UI flicker during chat insertion -- but note the anti-pattern: content scrolled out of DECSTBM regions does NOT enter native scrollback, so this is only useful for the insert_before operation, not for general scrolling)

### 2. RNK spike (tk-add8) -- proceed with eyes open

The spike is worth doing because rnk's architecture (React-like components, flexbox layout, line-level diffing) could reduce the maintenance burden of the bottom UI rendering code. But:

- **Scope the spike narrowly:** Render the bottom UI (input + progress + status) via rnk inline mode. Keep chat history insertion as custom crossterm code.
- **Evaluate against real bugs:** Does rnk's renderer actually fix the resize/autowrap/cursor bugs in `ai/review/tui-manual-checklist-2026-02.md`? If not, it is just moving complexity.
- **Have a kill criteria:** If rnk's inline renderer has its own resize bugs or if the dependency is too unstable, abandon the spike early.
- **Do not adopt rnk for chat rendering.** Ion's chat -> scrollback insertion pattern has no equivalent in rnk's component model.

### 3. Do not adopt any other crate

| Crate     | Action | Reason                                                 |
| --------- | ------ | ------------------------------------------------------ |
| ratatui   | Skip   | Wrong viewport model, resize broken in inline mode     |
| r3bl_tui  | Skip   | Massive deps, REPL mode too limited, no hybrid pattern |
| tui-realm | Skip   | Ratatui wrapper, inherits all limits                   |
| cursive   | Skip   | Fullscreen only                                        |
| termwiz   | Skip   | Crossterm alternative, no hybrid abstractions          |
| rxtui     | Skip   | Fullscreen virtual DOM, too early                      |

### 4. Watch list

| Crate/Project                | Why Watch                                                              | Trigger                      |
| ---------------------------- | ---------------------------------------------------------------------- | ---------------------------- |
| **ratatui Viewport::Inline** | If PR #1964 (dynamic height) and #2355 (resize fix) merge, re-evaluate | Both PRs merged and released |
| **rnk**                      | If downloads reach >1000/week and API stabilizes                       | Stable 1.0 release           |
| **Gemini CLI's opentui**     | If extracted as a standalone Rust crate                                | Published to crates.io       |
| **Claude Code's renderer**   | If Anthropic open-sources the differential renderer as a library       | Published / documented       |

---

## Sources

### Crate Registries

- [rnk on crates.io](https://crates.io/crates/rnk) -- v0.17.3, 158 total downloads
- [rnk docs.rs](https://docs.rs/rnk/0.17.3/rnk/) -- API reference
- [rnk GitHub](https://github.com/majiayu000/rnk) -- source code
- [r3bl_tui on crates.io](https://crates.io/crates/r3bl_tui) -- v0.7.6, ~8k DL/mo
- [r3bl_tui docs.rs](https://docs.rs/r3bl_tui/latest/r3bl_tui/)
- [tuirealm on crates.io](https://crates.io/crates/tuirealm) -- v3.3.0
- [cursive on lib.rs](https://lib.rs/crates/cursive) -- v0.21.1, ~24k DL/mo
- [termwiz on crates.io](https://crates.io/crates/termwiz) -- v0.23.3, 3.6M total
- [rxtui docs.rs](https://docs.rs/rxtui/0.1.8/rxtui/)

### Ratatui Inline Viewport Issues

- [ratatui #984: Allow Inline Viewport resize](https://github.com/ratatui/ratatui/issues/984)
- [ratatui #1426: insert_lines_before](https://github.com/ratatui/ratatui/issues/1426)
- [ratatui #1964: set_viewport_height PR](https://github.com/ratatui/ratatui/pull/1964)
- [ratatui #2086: Horizontal resize breaks inline](https://github.com/ratatui/ratatui/issues/2086)
- [ratatui #2167: Widget positioning breaks in large terminals](https://github.com/ratatui/ratatui/issues/2167)
- [ratatui inline example](https://ratatui.rs/examples/apps/inline)

### Agent TUI Architecture

- [Claude Code flickering fix (HN)](https://news.ycombinator.com/item?id=46699072) -- chrislloyd (Anthropic) on differential renderer
- [Claude Code #769: Screen flickering](https://github.com/anthropics/claude-code/issues/769)
- [Claude Code #18299: Scroll position issues](https://github.com/anthropics/claude-code/issues/18299)
- [Gemini CLI rendering blog](https://developers.googleblog.com/making-the-terminal-beautiful-one-pixel-at-a-time/)
- [Gemini CLI #18703: Double rendering in shpool](https://github.com/google-gemini/gemini-cli/issues/18703)
- [pi-mono repository](https://github.com/badlogic/pi-mono) -- pi-tui differential rendering
- [opencode #4032: Gaps in chat history](https://github.com/sst/opencode/issues/4032)

### Terminal Rendering

- [Synchronized Output spec](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036)
- [crossterm BeginSynchronizedUpdate](https://docs.rs/crossterm/latest/crossterm/terminal/struct.BeginSynchronizedUpdate.html)
- [Boris Cherny on viewport vs scrollback](https://www.threads.com/@boris_cherny/post/DSZbaGaiEwX/)

### Prior Ion Research

- `ai/research/inline-tui-patterns-2026.md`
- `ai/research/tui-hybrid-chat-libraries-2026.md`
- `ai/research/inline-tui-rendering-deep-dive-2026.md`
- `ai/research/ratatui-vs-crossterm-v3.md`
- `ai/design/tui-v3-architecture-2026-02.md`
- `ai/design/runtime-stack-integration-plan-2026-02.md`
