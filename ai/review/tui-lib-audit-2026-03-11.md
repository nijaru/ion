# TUI Lib Audit

**Date:** 2026-03-11  
**Branch:** `tui-work`  
**Scope:** `crates/tui` inline rendering contract and `IonApp` footer integration

## Findings

### P1: Inline reserve and rendered-height semantics are mixed

`Terminal` anchors inline mode using the requested reserve height, while `AppRunner` clears/shrinks using actual content height. This is valid only if the app treats the reserve as a real footer region and keeps layout within it. The fixed 10-row reserve hack violated that by leaving permanent dead space and by not defining when reserve should grow or reset.

**Required contract:** reserve grows with the current draft, footer content renders within that reserve, and shrink leaves slack below the visible footer until reset.

### P1: Footer layout can overflow the reserved region

The custom prompt-box footer computed input height directly from visual lines with no cap against the active reserve. Once multiline input exceeded the available rows, later footer children received out-of-bounds areas and panicked during rendering.

**Required fix:** compute footer layout once, cap visible input height to `reserve - fixed_rows`, and render only the visible tail when input exceeds the reserve.

### P1: Buffer/write coordinate usage is easy to misuse

`Buffer` writes are buffer-local, while layout rectangles are absolute within the root buffer. The current custom `Canvas` footers made this easy to get wrong. A previous footer patch only appeared to work after switching to `0,0` writes, which masked the real overflow bug.

**Required fix:** keep all custom renderers aligned to the buffer contract, add tests that prove `Buffer::diff` offsets by `area.x/area.y`, and prefer layout helpers over ad hoc row math.

### P2: Footer geometry is still too implicit

`IonApp` currently recomputes footer rows and input/cursor behavior in separate helpers. This makes it easy for render, cursor, and reserve logic to diverge.

**Required fix:** use one small `FooterLayout` value per frame and derive render rows and cursor clipping from it.

### P1: Multiline growth still duplicates prompt/border rows on first expansion

User validation after the reserve-contract pass showed that shrink behavior improved, but the first multiline growth path still duplicates prompt/border rows in the entry box. This means the footer reserve model is not yet fully correct even after the overflow clamp and reset semantics were introduced.

**Required fix:** audit the growth path specifically: visible-tail selection, footer child ordering, reserve growth timing, and redraw/clear behavior while the composer transitions from one visible row to multiple rows.

### P2: Panic/sync-update cleanup must remain terminal invariants

The rewrite already hit a panic path in footer rendering. This confirms the older render-safety reviews were correct: panic cleanup and synchronized output lifecycle have to be owned by the terminal/runtime layer, not patched piecemeal in call sites.

## Immediate Implementation Priorities

1. Lock the footer reserve contract and close the remaining multiline growth duplication bug (`tk-ajlv`).
2. Encode coordinate and layout invariants as tests in `crates/tui` (`tk-s2ib`).
3. Run PTY/manual parity checklist once multiline, resize, and footer placement are stable (`tk-9yt1`).
4. Only after that, move to session/display ownership cleanup (`tk-43cd`).
