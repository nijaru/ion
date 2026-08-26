# Ion terminal contract

This document owns the terminal substrate and TUI architecture: terminal
lifecycle, input negotiation, capabilities, surfaces, inline rendering, and
the frontend's state/reducer structure. The shared frontend/runtime contract
remains in `DESIGN.md` §21. The implementation owners are the internal
`ion-terminal` crate and the `ion` TUI.

## Ownership

`TerminalSession` is the only owner of process terminal state. It owns raw
mode, bracketed paste, keyboard enhancement flags, output capture, input
stream creation, and restoration. `Drop` is a last-resort cleanup path;
normal shutdown calls `restore` and propagates its error. The panic hook
restores the terminal before the previous hook prints its diagnostic.

`ion` owns `UiState`, reducer messages, runtime effects, transcript
projection, and the choice of what to render. It does not enable terminal
modes, decode Crossterm events, or write terminal protocol sequences.

`TerminalSession::suspend` and `resume` are idempotent lifecycle transitions.
Resume re-enters the requested modes and refreshes negotiated capability
state. A frontend must repaint after resume because physical renderer state
may no longer describe the terminal.

Job control follows the same ownership rule. On suspend, the terminal claim
is released first — cooked modes restored, negotiated capabilities
disabled — before the process stops via the default SIGTSTP disposition.
On wake (SIGCONT or return), modes are re-armed and the next frame is a full
repaint. Raw mode disables ISIG, so a user's Ctrl+Z arrives as the `0x1a`
key byte and drives the same path deterministically. In orphaned process
groups the kernel discards the re-raised stop signal; every step is
idempotent so both outcomes are correct.

## Requirements and capabilities

`TerminalRequirements` is declarative input to session activation:

- inline surface;
- bracketed paste enabled;
- Kitty keyboard enhancement requested;
- synchronized output disabled by default;
- focus and mouse reporting disabled by default.

`CapabilitySupport` is `Unknown`, `Unsupported`, or `Supported`. A capability
probe never becomes an implicit claim of support: a failed keyboard probe is
recorded as `Unknown`, while an explicit negative response is
`Unsupported`. Current activation probes the Kitty keyboard protocol and,
when supported, enables only `DISAMBIGUATE_ESCAPE_CODES`; it then restores the
protocol with the matching pop command. `modifyOtherKeys` remains separately
tracked and is not inferred from Kitty support.

Synchronized output is an explicit requirement, not a default assumption.
When requested, `TerminalSession::render` wraps one complete `Screen` frame
in the terminal's synchronized-update sequence. Unsupported terminals may
ignore that private mode; the renderer's correctness does not depend on it.

## Typed input

`InputStream` is the sole terminal reader for a live frontend. It converts
Crossterm events into the crate-owned `InputEvent` vocabulary:

- `Key(KeyEvent)` with application-owned key codes and modifier flags;
- `Paste(String)` as one semantic payload, including embedded newlines;
- `Focus`, `Resize`, and typed `Mouse` events;
- `TerminalResponse` for negotiated responses that need a future owner;
- `Closed` as the terminal-boundary vocabulary for a closed reader.

The UI must not parse escape sequences or inspect Crossterm state. Shift+Enter
remains a distinct typed key when the terminal reports the modifier, and
multiline paste is never split into submit events by the terminal boundary.
Unsupported keyboard enhancement falls back to the normal terminal event
decoder and leaves the capability decision explicit.

## Surface and inline rendering

`Surface` owns the virtual cell buffer and dimensions used to compose a frame.
`Screen` owns physical policy: the launch origin, the visible window, cursor
translation, row diffing, and committed-history scrolling. `Frame` separates
immutable committed rows from the mutable live band.

The renderer contract is:

1. The host writes the banner and anchors the live region at the launch
   cursor. Content above that point is native terminal scrollback.
2. Physical scrolling occurs only when committed history advances. Live-band
   growth, shrinkage, and preview toggles repaint in place.
3. Resize or invalid physical state discards the previous window and repaints
   every row. Freed rows are blanked.
4. The cursor maps from an absolute wrapped row to the visible row by the
   current window offset. A cursor outside the window stays hidden.
5. Width calculation, wrapping, cursor columns, and cell emission use the
   same display-width model. Rows are emitted as complete rows so wide
   characters and grapheme clusters cannot be split by a partial diff.

The committed transcript is logical content. The frontend rewraps it from
that content after a width change; it does not rewrite already-committed
terminal scrollback as mutable UI state. This is the resize/reflow policy:
committed scrollback is immutable once emitted; only the logical transcript
is rewrapped.

## TUI architecture

Ion renders its TUI through its own line-diff screen layer over crossterm;
committed scrollback and the live composer/status region are one growing
line array, each frame diffed against the previous. Ion owns application
state/update semantics; the substrate boundary is everything above.

### One UI state owner

One top-level `UiState` / reducer-style update path. Widgets receive state
and emit UI intents; they do not own independent agent runtimes or hidden
durable state.

```text
RuntimeEvent | KeyEvent | Resize | Tick
        ↓
update(UiState, UiMessage)
        ↓
new UiState + UiEffect
        ↓
render(UiState)
```

### Runtime interaction

UI effects call `SessionHandle`; responses return as messages/events. Model
changes follow this path; a frontend never owns or swaps the provider
directly.

The UI never blocks rendering on provider/tool/metadata I/O. Cache locks are
not step-boundary synchronization and MUST NOT span provider futures.

### Inline-first

The Pi-like inline experience is the first-class mode: completed transcript
stays useful native terminal scrollback while the live composer/status area
redraws efficiently. The acceptance contract is the renderer contract in
"Surface and inline rendering" above; no separate inline semantics exist.

### Restoration and shutdown

Terminal raw-mode/mouse/paste/keyboard protocol changes use an RAII guard
plus a final panic/error restoration path. Runtime shutdown and terminal
restoration have one owner each; cleanup is not scattered across widgets.

## Acceptance checks

The contract is checked at the owning boundary:

- `ion-terminal` unit tests cover typed Shift+Enter, semantic multiline
  paste, synchronized-update framing, virtual-surface rows, wide characters,
  cursor translation, physical committed scrolling, live-band growth, and
  resize invalidation.
- Ion PTY tests cover clean exit, panic restoration, and slash-command
  rendering through the same session owner.
- Workspace gates are `cargo fmt --check`, workspace Clippy with warnings
  denied, and `cargo test --workspace`.
- Real tmux checks remain required for width/reflow and resume history.
- Maintainer live dogfood is still required for the H0a acceptance verdict;
  automated green tests do not close that human gate.

The automated substrate work is complete through keyboard/capability
negotiation, synchronized output, virtual-surface, PTY restoration, and resize
regressions. Real-terminal checks and the maintainer's H0a/H0b dogfood verdict
remain open, as do any concrete defects found there. Do not claim terminal
completion from automated gates alone.
