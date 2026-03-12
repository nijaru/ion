# Bubble Tea v2 vs Custom Rust TUI Host

**Date:** 2026-03-12  
**Task:** `tk-n6f7`  
**Question:** Should ion continue building a custom Rust TUI host, or switch to Go with Bubble Tea v2?

## Executive Summary

Bubble Tea v2 materially changes the TUI decision.

- **Bubble Tea v2 is now a serious host framework** for inline CLI agents, with official emphasis on inline mode, compositing, and improved input/render performance.
- **ion's current custom Rust TUI is failing in the exact runtime layer Bubble Tea is strongest at**: inline region ownership, redraw/clear contracts, resize behavior, and multiline composer behavior.
- **Rust is not disproven for the core/runtime**, but the current evidence no longer supports "custom Rust TUI by default" as the obvious best path.
- **The real decision is now between:**
  1. **All Go**: rewrite ion around Bubble Tea v2
  2. **Go TUI host + Rust core**
  3. **Continue custom Rust TUI**

Current leaning: **All Go is a credible default if the goal is one coherent product and the TUI is the current bottleneck.**  
Secondary option: **Go TUI host + Rust core** if preserving Rust agent/runtime code still matters.  
Weakest option: **continue the current custom Rust TUI path without a much harder reset.**

## Current External Facts

### 1. Bubble Tea v2 is a real upgrade

Charm's official Bubble Tea v2 announcement says v2 is built around:

- optimized rendering
- compositing
- improved input handling
- inline mode as a first-class use case
- production use in Charm's own coding agent (`Crush`)

This is directly relevant to ion's hybrid "chat in scrollback + anchored bottom UI" pattern.

**Sources**

- [Charm v2 announcement](https://charm.land/blog/v2/)
- [Bubble Tea repository](https://github.com/charmbracelet/bubbletea)
- [Bubble Tea v2.0.2 release](https://github.com/charmbracelet/bubbletea/releases/tag/v2.0.2)

### 2. Bubble Tea v2 does not eliminate all host work

Bubble Tea solves a large share of terminal UI infrastructure:

- event loop
- renderer
- component/update model
- input handling
- ecosystem widgets (`bubbles`)

But ion would still need custom logic for:

- transcript vs ephemeral footer separation
- ACP session/event mapping
- agent-specific scrollback behavior
- approvals and tool lifecycle views
- memory/session inspection

So Bubble Tea v2 solves **most** of the terminal UI problem, not all of the product.

### 2.1 The Bubbles ecosystem covers a meaningful share of ion's current pain

The official `bubbles` components already include:

- `textinput`
- `textarea`
- `viewport`
- spinner/list/progress/form-like components

Relevant current component capabilities:

- `textarea` supports multiline input, unicode, paste handling, and vertical scrolling
- recent releases improved textarea performance and fixed paste/multiline corruption issues
- `viewport` now has more mature scrolling support

This does not give ion a full coding-agent UI for free, but it removes a large amount of low-level composer/viewport work that ion is currently implementing itself.

**Sources**

- [Bubbles repository](https://github.com/charmbracelet/bubbles)
- [Bubbles releases](https://github.com/charmbracelet/bubbles/releases)

### 3. ACP is mature enough to use as an architectural boundary

The current protocol defines:

- initialization
- session creation/load
- prompt submission
- session update streaming
- cancel
- tool calls
- plans
- file operations
- permissions / elicitation

That is enough to use ACP as a real session/event boundary in ion.

**Sources**

- [ACP overview](https://agentclientprotocol.com/protocol/overview)
- [ACP schema](https://agentclientprotocol.com/protocol/schema)
- [ACP tool calls](https://agentclientprotocol.com/protocol/tool-calls)
- [ACP plans](https://agentclientprotocol.com/protocol/agent-plan)
- [ACP elicitation RFD](https://agentclientprotocol.com/rfds/elicitation)

### 4. ACP library support currently favors Rust

As of 2026-03-12:

- ACP publishes an official **Rust** library
- Go appears under **community libraries**

This is evidence that **Rust still has an advantage on the agent/runtime/protocol side**, even if Go has the stronger TUI host story.

**Sources**

- [ACP Rust library](https://agentclientprotocol.com/libraries/rust)
- [ACP community libraries](https://agentclientprotocol.com/libraries/community)

## What ion's Current Rust TUI Is Actually Struggling With

The recent `tui-work` investigation shows the problem is not "Rust can't do this." The problem is that ion is currently building its own host runtime while also building the product.

### Confirmed problem classes

1. **Inline reserve vs rendered-height drift**
- reserve height and actual content height were owned by different layers

2. **Frame-buffer vs terminal coordinate confusion**
- local buffer coordinates, frame-absolute layout coordinates, and terminal-global cursor offsets were too easy to mix

3. **Footer/composer geometry drift**
- layout, render rows, cursor placement, and visible-line clipping were recomputed in different places

4. **Runtime invariants were under-specified**
- `Inline -> Inline` growth/shrink
- resize behavior
- stale row clearing
- synchronized update lifecycle
- panic cleanup

5. **Too much host behavior lives in app code**
- `IonApp` still carries layout/runtime-adjacent responsibilities that should live in a stronger framework boundary

**Sources**

- [TUI audit 2026-03-11](/Users/nick/github/nijaru/ion/ai/review/tui-lib-audit-2026-03-11.md)
- [TUI v3 architecture program](/Users/nick/github/nijaru/ion/ai/design/tui-v3-architecture-2026-02.md)
- [Hybrid TUI library research](/Users/nick/github/nijaru/ion/ai/research/tui-hybrid-chat-libraries-2026.md)

## Why `rnk` Felt Better

`rnk` worked better mainly because it had a **smaller blast radius**:

- bottom-area rendering only
- less custom terminal ownership
- fewer lifecycle contracts owned by ion

Once ion moved to a more ambitious custom TUI layer, it took ownership of:

- terminal mode switching
- inline region management
- buffer diffing
- layout contracts
- footer/composer redraw behavior
- resize and PTY edge cases

So "rnk felt better" does **not** prove `rnk` is the final answer. It mostly proves the narrower path was more stable.

## Comparison

| Option | Pros | Cons | Current Read |
| --- | --- | --- | --- |
| **All Go (Bubble Tea v2)** | One language, strongest host/UI ergonomics, easier LLM-assisted development, likely fastest path to a reliable TUI | Rewrite everything, lose current Rust runtime investment, ACP/runtime code moves to weaker official library ecosystem | **Strong option** |
| **Go TUI + Rust core** | Best UI ergonomics plus Rust runtime/protocol strengths, clean long-term host/runtime separation | More build/package complexity, IPC/process boundary, two-language codebase | **Strong option** |
| **Custom Rust TUI** | Maximum control, one language if whole app stays Rust, easiest integration with existing code | We still own the hardest terminal host problems, current lib is not yet trustworthy, more agent friction when building UI infra | **Weakest current option** |

## Implications for ACP and Native Agent Design

ACP should not be treated as a thin plugin.

The right long-term model is:

- **native ion agent**: owns memory, providers, tools, swarms, subagents, RLM patterns
- **hosted external agents**: Claude / Gemini / Codex / others via ACP or adapters
- **TUI host**: transcript, composer, approvals, session selection, mode/status, inspection UI

This means the TUI should talk to an **agent session interface**, not to providers directly.

That interface should support:

- native ion sessions
- external ACP sessions

Whether ion is all-Go or split Go/Rust, this architectural boundary still makes sense.

## What Bubble Tea v2 Is Worth Studying Even If ion Stays Rust

If ion keeps a custom Rust TUI, Bubble Tea v2 should still inform the design:

1. **single-owner rendering/compositing**
2. **inline-mode lifecycle**
3. **component/update discipline**
4. **input/focus handling**
5. **clearer boundaries between app state and terminal runtime**

The question is no longer "is Bubble Tea relevant?" It clearly is.

## Provisional Conclusion

Current evidence does **not** support continuing the current custom Rust TUI as the default path.

The live decision should now be between:

1. **All Go with Bubble Tea v2**
2. **Go Bubble Tea v2 host + Rust agent/runtime**

If the primary goal is:

- **one coherent codebase and fastest path to a good TUI** -> prefer **All Go**
- **stronger runtime/protocol implementation with a clean host boundary** -> prefer **Go TUI + Rust core**

What should not happen:

- keep spending large effort on the current custom Rust TUI without first comparing it directly against Bubble Tea v2's model

## Next Research Questions

1. How complex is a real Bubble Tea v2 inline coding-agent host in practice?
2. Does Bubble Tea v2 actually solve ion's multiline composer + scrollback + footer interaction well, or only most of it?
3. If ion went all-Go, what is the concrete rewrite surface for:
   - providers
   - sessions
   - tools
   - memory
   - MCP/ACP integration
4. If ion split Go TUI / Rust core, what is the cleanest session/event boundary?

## Sources

### Primary external

- [Charm v2 announcement](https://charm.land/blog/v2/)
- [Bubble Tea repository](https://github.com/charmbracelet/bubbletea)
- [Bubble Tea v2.0.2 release](https://github.com/charmbracelet/bubbletea/releases/tag/v2.0.2)
- [ACP overview](https://agentclientprotocol.com/protocol/overview)
- [ACP schema](https://agentclientprotocol.com/protocol/schema)
- [ACP Rust library](https://agentclientprotocol.com/libraries/rust)
- [ACP community libraries](https://agentclientprotocol.com/libraries/community)

### ion-local

- [TUI audit 2026-03-11](/Users/nick/github/nijaru/ion/ai/review/tui-lib-audit-2026-03-11.md)
- [TUI v3 architecture program](/Users/nick/github/nijaru/ion/ai/design/tui-v3-architecture-2026-02.md)
- [Hybrid TUI library research](/Users/nick/github/nijaru/ion/ai/research/tui-hybrid-chat-libraries-2026.md)
- [Inline TUI patterns](/Users/nick/github/nijaru/ion/ai/research/inline-tui-patterns-2026.md)
