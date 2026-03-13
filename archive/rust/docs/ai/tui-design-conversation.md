
If you were to create a tui library crate for rust similar to other languages that is state of the art and high performance with great ux, what would that look like? Compare ratatui, rnk, ink, bubble tea, etc. preferably this theoretical one we are designing supports both inline and alternative view fully, can make Claude code like tui or even something like neovim or tmux or zellij or btop

Feb 22

Architected comprehensive TUI library design comparing existing frameworks and solutions
Architected comprehensive TUI library design comparing existing frameworks and solutions







Great design problem. Let me first be precise about the comparison landscape, then sketch out what a genuinely state-of-the-art Rust TUI crate would look like.

Honest Comparison of the Existing Field
Ratatui (Rust) The current Rust standard. Immediate-mode rendering with a double-buffered diff engine — you describe the full UI every frame, it diffs against the previous buffer and only writes changed cells. Solid backend abstraction (crossterm, termion, termwiz). Weaknesses: no retained state, no layout engine beyond a simple constraint solver, no async-native design, widgets are not composable components — they're just functions. Building something like Zellij on top of it requires a lot of scaffolding you write yourself.

Bubbletea (Go) Elm architecture: Model → Msg → Update → View. Clean, testable, predictable. The Charm ecosystem (lipgloss for styling, bubbles for components) is genuinely pleasant. Weakness: the Elm model gets unwieldy at scale — nested updates become boilerplate hell. No retained mode, no real layout engine, limited inline support.

Ink (Node.js/React) React component model with Yoga (Facebook's flexbox engine) for layout. This is actually the most ergonomic of the existing options for component composition. Hooks, reconciler, diffing — all the React ideas apply. Weaknesses: JavaScript, single-threaded, performance ceiling, not suitable for something like Neovim.

Textual (Python) The most ambitious in terms of DX. CSS-like styling, a real layout engine, reactivity system, both inline and fullscreen, async-native (asyncio). Closest to "web dev in the terminal." Weakness: Python, so performance ceiling, and CSS-in-TUI has edge cases.

"rnk" — I want to flag that I'm not confident what you're referring to here. Could be rink (Dioxus-based TUI renderer), or a lesser-known crate, or possibly a typo. If you clarify I can include it properly.

termwiz (Rust, from Wez — wezterm author) Underrated. Lower level than ratatui, but extremely capable — handles sixel, unicode segmentation, hyperlinks, mouse reporting modes, multiplexer protocol. This is what wezterm is built on. Relevant to the "zellij-tier" ambition.

What a State-of-the-Art Rust TUI Crate Would Actually Look Like
Call it vex or terracotta for the sake of discussion. The core thesis: retained component tree + async event loop + real layout engine + multiplexer primitives as first-class citizens.

1. Architecture: Hybrid Retained/Immediate
Neither pure immediate-mode (ratatui) nor pure VDOM (ink) is ideal. The right answer is a reactive retained tree with dirty-tracking, similar to what Leptos or Dioxus do for the web:

rust
// Component definition — declarative, composable
#[component]
fn FilePane(cx: Scope, path: PathBuf) -> impl View {
    let entries = use_async(cx, || read_dir(path.clone()));
    let selected = use_signal(cx, || 0usize);

    list(entries.read().iter().enumerate().map(|(i, entry)| {
        list_item(entry.name())
            .selected(i == *selected.read())
            .on_key(Key::Enter, move |_| open(entry.clone()))
    }))
    .scrollable()
    .border(Border::Rounded)
}
Signals propagate changes. Only subtrees with dirty signals re-render. The framework diffs the output against the terminal buffer — two levels of diffing, component tree and cell buffer.

2. Layout Engine: Taffy (not a custom solver)
Taffy is a production-grade Rust flexbox/grid layout engine already used by Dioxus and Bevy UI. No reason to reinvent this. You get:

Flexbox
CSS Grid
Block layout
Absolute positioning
This is what makes something like Zellij's pane management or btop's dashboard layout tractable without hand-rolling a constraint solver.

rust
row()
    .gap(1)
    .children([
        sidebar().flex_basis(30).flex_shrink(0),
        main_content().flex_grow(1),
        detail_pane().flex_basis(40).flex_shrink(1),
    ])
3. Async-First, Tokio-Native
Events, I/O, timers — everything flows through a tokio runtime. Components can await things without blocking the render loop. The event loop runs on a dedicated thread with a channel bridge:

rust
let app = App::new()
    .fps(60)                          // or event-driven only
    .on_resize(|size| ...)
    .run(root_component)
    .await?;
Critically: back-pressure aware. If rendering falls behind, frames are dropped gracefully. This is what separates something that can be btop (60fps charts) from something that can't.

4. Both Inline and Fullscreen — Properly
Most libraries treat these as an afterthought. They're fundamentally different terminal modes:

Fullscreen (alternate screen): smcup/rmcup, you own the whole screen, no scrollback contamination
Inline: You occupy N lines below the cursor, scroll up as needed, restore on exit — like how fzf or Claude Code's UI works
The library should make this a top-level concern with distinct layout roots:

rust
// Inline — occupies up to 20 lines, expands as needed
App::inline(20).run(my_component).await?;

// Fullscreen — takes over the terminal
App::fullscreen().run(my_component).await?;
Inline mode requires careful cursor math and scroll management that ratatui doesn't handle well currently.

5. Multiplexer Primitives (Zellij/Tmux tier)
For something to reach Zellij's level, you need first-class:

Pane: owns a rectangular region, independent scroll/render state, can host a real PTY via portable-pty
Virtual terminal emulator: to render the output of a subprocess inside a pane (this is what makes an embedded shell possible) — vt100 or alacritty_terminal crates handle the VT state machine
Focus graph: tab order, pane focus, modal overlays — explicit not implicit
IPC layer: for attaching multiple clients to one session (tmux-style)
rust
// A split-pane terminal emulator in ~20 lines of actual logic
workspace()
    .split(Direction::Horizontal, [
        terminal_pane(Pty::spawn("nvim")?).weight(0.6),
        terminal_pane(Pty::spawn("bash")?).weight(0.4),
    ])
6. Styling: Cascading, Themeable
Not just per-widget colors. A proper cascade:

rust
// Theme defined at root, inherited down
theme! {
    border_color: Color::Rgb(80, 80, 120),
    text: Color::White,
    selected_bg: Color::Rgb(40, 40, 80),
    
    [focused] {
        border_color: Color::Cyan,
    }
}
Widgets query the nearest theme in their scope. Override locally where needed.

7. Backend Abstraction
Pluggable backends, each optimized:

Crossterm (default, cross-platform)
Termios (unix, lower overhead)
Termwiz (for sixel, hyperlink, advanced protocol support)
Test backend (deterministic output for snapshot testing — ratatui has this, it's genuinely great for testing)
Web backend (WASM + xterm.js — Textual is doing this, it's powerful)
8. Input Model
Unified input handling with a proper key/event model that covers:

Kitty keyboard protocol (modifiers on every key, no ambiguity)
Mouse: click, drag, scroll, hover (with terminal capability negotiation)
Bracketed paste
Focus in/out events
Resize
rust
on_key(Key::Ctrl('p'), |cx| {
    cx.open_modal(CommandPalette::new());
})
What Makes This Genuinely Different From Ratatui
Concern	Ratatui	Theoretical vex
Component model	None (manual state)	Reactive signals, dirty tracking
Layout	Custom constraint solver	Taffy (flexbox/grid)
Async	Bolted on externally	First-class tokio integration
Inline mode	Partial	Full, with scroll math
PTY/subprocess panes	Not in scope	First-class
Styling	Per-widget	Cascading theme system
Testing	Good (test backend)	Great (snapshot + behavior)
Neovim-tier possible?	Barely	Yes
Zellij-tier possible?	With heroic effort	Designed for it
What's Genuinely Hard
Be skeptical of anyone who says this is easy to build:

Unicode is a nightmare. Cell width calculation (East Asian wide chars, ZWJ emoji sequences, combining characters) is still not fully solved in any TUI library. You need unicode-width + unicode-segmentation + careful handling of terminals that disagree on widths.

Inline mode scroll math is subtle. Getting the cursor back to the right place after a resize while in inline mode is genuinely tricky.

The reactive/signal approach adds complexity. Leptos-style fine-grained reactivity in a TUI context means you need to carefully define "what counts as a render boundary" — get it wrong and you either over-render or have stale UI.

PTY + VT emulation is a rabbit hole. The VT100/VT220/xterm spec is enormous and terminals disagree on behavior constantly.

The closest existing thing to this vision is actually Dioxus with its TUI renderer (dioxus-tui) — it uses the reactive signal model and Taffy layout, but the TUI backend isn't production-ready. That project's architecture is probably the right north star to study.

rnk is a rust crate for tui apps similar to ratatui. I’m using it right now but curious about just making my own

Feb 22

Pivoted strategy to locate unfamiliar Rust crate information
Pivoted strategy to locate unfamiliar Rust crate information






I can't find rnk anywhere — not on crates.io, not in search results. I want to be upfront about that rather than pretend I know it. It's possible it's:

A very new/obscure crate with minimal indexing
An internal or private crate you're using
A name I'm misremembering slightly (could it be rink, rank, ink-rs, or something else?)
Can you drop the crates.io link or repo URL? I'd rather look at the actual thing than guess, especially since you're actively using it and we're potentially designing something to supersede or learn from it.

That said, on the "should I just make my own" question — here's the honest take:

Yes, if your use case is specific enough that ratatui/rnk is making you work around it constantly. The things most worth building from scratch are:

The event loop + async integration — this is where most existing crates leave you holding the bag. A clean tokio-native event pump with back-pressure is genuinely worth owning.
Your own component abstraction layer on top of ratatui's rendering primitives — you don't have to replace the diff/render engine, that part ratatui does well. Just build a retained component tree above it.
No, if the friction is mostly layout and state management — those are solvable with patterns on top of ratatui, and rewriting the terminal I/O layer, unicode handling, and backend abstraction is a significant time sink for not much gain.

The sweet spot for most projects is: ratatui for the buffer/render layer, your own architecture above it. What specifically is frustrating you about rnk right now?

https://docs.rs/rnk/latest/rnk/ use case is https://github.com/nijaru/ion

Feb 22

Evaluated framework suitability for coding agent TUI application
Evaluated framework suitability for coding agent TUI application









Okay, now I have a clear picture. Let me be direct and specific.

What rnk actually is
Good news: rnk already implements most of what I described as theoretical in my previous response. It's not widely known, but looking at its deps and API it has:

Taffy for flexbox layout (exactly what I recommended)
Tokio native
True inline + fullscreen with runtime switching
React hooks model (use_state, use_input, etc.)
Reconciler — so it's doing retained-mode diffing, not pure immediate-mode like ratatui
Cross-thread request_render() for streaming updates
Command system for side effects
That's a legitimately solid foundation. The prior discussion of "someone should build this" — rnk is already building it.

For ion specifically: should you roll your own?
No, not yet. Here's why: the hard infrastructure (Taffy, tokio event loop, inline mode cursor math, reconciler) is already in rnk. Rolling your own means re-solving those problems. What you'd actually be building is the application-layer widgets on top of any framework anyway.

What ion actually needs that's above the TUI framework layer:

Streaming text rendering — LLM token streams at potentially hundreds of tokens/sec. The key question is whether request_render() + signal updates can keep up without flickering or starving the event loop. This is worth profiling with rnk before abandoning it.

Syntax-highlighted code blocks — needs syntect or tree-sitter highlights mapped to terminal colors. This is your widget to build regardless of framework.

Diff/patch views — side-by-side or unified diffs with line highlighting. Again, application-layer.

Multiline input — does rnk have a robust multiline text input component? This is often where frameworks fall short for coding agents. Worth checking — if it doesn't, that's a real gap since you need vim-like editing for long prompts.

Scrollable conversation history — long sessions with lots of tool calls need virtualized scrolling. If rnk's list component doesn't do virtual scrolling, you'll hit perf issues at long contexts.

The one legitimate reason to roll your own: if you want ion's UI to be a reusable library that other people's coding agents can use. In that case, owning the stack makes sense architecturally. But that's an org/distribution decision, not a technical one.

Honest gaps to investigate in rnk before deciding
These are the things I'd stress-test before committing either way:

1. Does use_state re-render performance hold up with 200+ messages in a conversation list?
2. Is there a virtualized/windowed list, or does it render all children?
3. What's the multiline text input story?
4. How does inline mode handle terminal resize mid-stream?
5. Does request_render() coalesce rapid calls, or does it spam redraws during fast token streaming?
If rnk answers those well, it's genuinely the right choice for ion and saves you months of work on infrastructure that isn't your differentiator. Your differentiator is the agent loop, tool execution, MCP integration, and the LLM provider abstraction — not the terminal rendering stack.

It does seem like there could be benefits to rolling our own

Feb 22

Weighed custom TUI library merits against existing alternatives
Weighed custom TUI library merits against existing alternatives


Yeah, let me think through this honestly rather than just advocate for either side.

Legitimate reasons for ion specifically
rnk is young and not widely battle-tested. v0.17.3 with 0 stars on the GitHub you linked for ion — you're already at the frontier. If rnk has a subtle bug in inline mode scroll math during resize, or request_render() coalescing issues under token streaming, you're filing issues and waiting rather than fixing it yourself. For a coding agent where the UX is the product, that's a real cost.

The hooks model may fight you. React-style hooks work great for discrete UI state. LLM streaming is a fundamentally different beast — you have a continuous byte stream, partial tool call JSON accumulating, multiple concurrent tool executions, a conversation tree with mutable history. Mapping that onto use_state/use_effect may feel increasingly awkward as ion gets more complex. You'd know this better than me given how far you've gotten.

You have a very specific widget set. Ion doesn't need charts, gauges, sparklines, progress bars. It needs maybe 6-8 widgets, all of them custom: streaming text renderer, code block with syntax highlighting, tool call accordion, diff view, multiline input, conversation list with virtual scroll, status bar. If you're writing all those custom regardless, the framework underneath matters less.

Performance ceiling is real. Syntect for syntax highlighting is expensive. Tree-sitter is expensive. Streaming token updates are frequent. If rnk's reconciler re-walks subtrees it shouldn't on every request_render(), you'll see it in practice at long sessions. Owning the render loop means you can make it event-driven and surgical — only re-render the streaming text block, leave the conversation history untouched.

What rolling your own actually means in practice
The scope matters a lot. There are three levels:

Level 1 — Just own your event loop and state, use crossterm directly for output. Skip rnk entirely, skip ratatui entirely. Write your own double-buffer cell diffing (it's ~300 lines), use Taffy directly for layout, crossterm for I/O. This is probably 2-3 weeks of focused work and gives you full control with no framework overhead. The "framework" for ion would be internal to the repo.

Level 2 — Same as above but extracted as a crate. You build it as ion-tui or similar, potentially publish it. Same technical work, more discipline required on API design since it becomes a public interface.

Level 3 — Build a general-purpose rnk competitor. This is a different project entirely. Probably not the right choice if ion is the goal.

For ion, Level 1 is probably the honest answer if you roll your own. It's not that much code, you understand your requirements exactly, and you avoid framework impedance mismatch.

The core of what you'd actually build
At minimum it's:

rust
// The whole "framework" might be this simple
struct Terminal { /* crossterm handle, two cell buffers */ }
struct Layout { /* Taffy tree */ }
struct EventLoop { /* tokio, crossterm events, your app channels */ }

trait Widget {
    fn layout_node(&self, taffy: &mut Taffy) -> NodeId;
    fn render(&self, area: Rect, buf: &mut Buffer);
    fn handle_key(&mut self, key: KeyEvent) -> Option<AppMsg>;
}
```

Then your widgets: `ConversationView`, `StreamingBlock`, `CodeBlock`, `InputBar`, `ToolCallView`, `StatusBar`. Each one a struct with real state, no hook magic. The conversation list does its own virtual scrolling. The streaming block gets a direct reference to the token receiver channel and renders what's accumulated.

The event loop is the part worth being careful about — you want:
```
tokio::select! {
    event = crossterm_event => handle_input(event),
    token = stream_rx => { append_token(token); mark_dirty(streaming_block_id); }
    _ = render_ticker.tick() => { if dirty { render(); clear_dirty(); } }
}
That dirty-flag approach with a capped render rate (say 60fps ceiling, but only render if something changed) is more precise than rnk's request_render() approach and trivial to implement yourself.

Honest recommendation
If rnk is currently causing you friction — fighting the hooks model, hitting performance issues, or working around missing features — then Level 1 is worth it and not actually that much work given the scope of ion's widget needs. You'd probably end up with a cleaner architecture for your specific problem.

If rnk is mostly working and the friction is just aesthetic or hypothetical, finish ion first, then extract and replace the TUI layer once you know exactly what you needed. The architecture will be cleaner for having built it once.

What's the actual friction point you're hitting right now?

I think owning the layer would be better

Feb 22

Architected custom TUI layer implementation strategy
Architected custom TUI layer implementation strategy

Good call. Let's design it properly. Given ion's use case, here's how I'd structure the layer from the ground up.

Crate structure
ion-tui/          (or keep it internal under src/tui/)
├── src/
│   ├── lib.rs
│   ├── terminal.rs      # raw terminal setup/teardown, crossterm wrapper
│   ├── buffer.rs        # cell grid, double-buffering, diff+flush
│   ├── layout.rs        # thin Taffy wrapper, your constraint types
│   ├── event.rs         # unified event type, crossterm input parsing
│   ├── app.rs           # event loop, render loop, dirty tracking
│   ├── style.rs         # Color, Style, theme tokens
│   └── widgets/
│       ├── mod.rs
│       ├── text.rs           # plain + styled text, word wrap
│       ├── block.rs          # border, title, padding container
│       ├── streaming.rs      # token stream renderer
│       ├── code_block.rs     # syntect-highlighted code
│       ├── conversation.rs   # virtual-scrolling message list
│       ├── input.rs          # multiline editor
│       ├── tool_call.rs      # collapsible tool execution view
│       ├── diff.rs           # unified diff renderer
│       └── status_bar.rs
Core types, bottom up
buffer.rs
The foundation — everything renders into this before hitting the terminal:

rust
#[derive(Clone, PartialEq)]
pub struct Cell {
    pub ch: char,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self { ch: ' ', style: Style::default() }
    }
}

pub struct Buffer {
    pub width: u16,
    pub height: u16,
    cells: Vec<Cell>,
}

impl Buffer {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cells: vec![Cell::default(); (width * height) as usize],
        }
    }

    pub fn get_mut(&mut self, x: u16, y: u16) -> &mut Cell {
        &mut self.cells[(y * self.width + x) as usize]
    }

    /// Compute the minimal set of crossterm commands to go from `prev` to `self`
    pub fn diff(&self, prev: &Buffer) -> Vec<DrawCommand> {
        let mut commands = Vec::new();
        let mut last_pos: Option<(u16, u16)> = None;
        let mut last_style: Option<Style> = None;

        for y in 0..self.height {
            for x in 0..self.width {
                let new = &self.cells[(y * self.width + x) as usize];
                let old = &prev.cells[(y * self.width + x) as usize];
                if new == old { continue; }

                // Only emit cursor move if not contiguous
                if last_pos != Some((x.saturating_sub(1), y)) {
                    commands.push(DrawCommand::MoveTo(x, y));
                }
                if last_style.as_ref() != Some(&new.style) {
                    commands.push(DrawCommand::SetStyle(new.style.clone()));
                    last_style = Some(new.style.clone());
                }
                commands.push(DrawCommand::Print(new.ch));
                last_pos = Some((x, y));
            }
        }
        commands
    }

    pub fn set_string(&mut self, x: u16, y: u16, s: &str, style: Style) {
        // Handle unicode width here — use unicode-width + unicode-segmentation
        let mut cx = x;
        for g in s.graphemes(true) {
            let w = g.width() as u16;
            if cx + w > self.width { break; }
            if let Some(ch) = g.chars().next() {
                let cell = self.get_mut(cx, y);
                cell.ch = ch;
                cell.style = style.clone();
                // Wide chars: fill next cell with space + same style
                if w == 2 && cx + 1 < self.width {
                    let next = self.get_mut(cx + 1, y);
                    next.ch = ' ';
                    next.style = style.clone();
                }
            }
            cx += w;
        }
    }
}
event.rs
Single unified event type — no leaking crossterm types into widget code:

rust
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    // ion-specific
    TokenReceived(String),
    ToolCallStarted { id: String, name: String },
    ToolCallCompleted { id: String, result: String },
    StreamingDone,
    Tick,   // if you want time-based animations
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

// KeyCode, KeyModifiers — your own enums, mapped from crossterm in one place
app.rs — the event loop
This is the heart. Dirty-flag driven, rate-capped:

rust
pub struct App<W: Widget> {
    terminal: Terminal,
    root: W,
    prev_buf: Buffer,
    dirty: bool,
    mode: RenderMode,
}

impl<W: Widget> App<W> {
    pub async fn run(mut self) -> Result<()> {
        let mut event_stream = EventStream::new();  // crossterm async stream
        let mut render_interval = tokio::time::interval(Duration::from_millis(16)); // 60fps cap
        render_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // Initial render
        self.render()?;

        loop {
            tokio::select! {
                // Terminal events (highest priority)
                Some(ev) = event_stream.next() => {
                    let ev = translate_event(ev?);
                    match &ev {
                        Event::Resize(w, h) => {
                            self.terminal.resize(*w, *h)?;
                            self.dirty = true;
                        }
                        Event::Key(k) if k.code == KeyCode::Char('c')
                            && k.modifiers.contains(KeyModifiers::CTRL) => break,
                        _ => {}
                    }
                    if self.root.handle_event(&ev) {
                        self.dirty = true;
                    }
                    if self.root.should_quit() { break; }
                }

                // Render tick — only paint if dirty
                _ = render_interval.tick() => {
                    if self.dirty {
                        self.render()?;
                        self.dirty = false;
                    }
                }
            }
        }

        self.terminal.restore()
    }

    fn render(&mut self) -> Result<()> {
        let size = self.terminal.size();
        let mut buf = Buffer::new(size.width, size.height);

        // Layout pass
        let layout = self.root.layout(size);

        // Render pass
        self.root.render(layout.area, &mut buf);

        // Diff + flush
        let commands = buf.diff(&self.prev_buf);
        self.terminal.flush_commands(commands)?;
        self.prev_buf = buf;
        Ok(())
    }
}
The Widget trait
Keep it simple. No trait objects in the hot path if you can avoid it:

rust
pub trait Widget {
    /// Called once or on constraint change. Returns computed Rect.
    fn layout(&mut self, available: Size) -> LayoutResult;

    /// Paint self into buf within area
    fn render(&self, area: Rect, buf: &mut Buffer);

    /// Return true if this event caused state change (triggers redraw)
    fn handle_event(&mut self, event: &Event) -> bool;

    /// Override if this widget initiates quit
    fn should_quit(&self) -> bool { false }
}
The widgets that matter for ion
streaming.rs — the critical one
This is where ion lives or dies. Token streaming at high throughput:

rust
pub struct StreamingText {
    /// Raw accumulated content — the source of truth
    content: String,
    /// Pre-wrapped lines for current width (cached, rebuilt on resize)
    wrapped: Vec<StyledLine>,
    last_width: u16,
    /// Track whether we're mid-codeblock for syntax state
    highlight_state: HighlightState,
    scroll_offset: usize,
    auto_scroll: bool,  // pin to bottom while streaming
}

impl StreamingText {
    pub fn push_token(&mut self, token: &str) {
        self.content.push_str(token);
        // Incremental re-wrap: only the last N lines need recomputing
        self.rewrap_tail();
    }
}
Key insight: never re-wrap the entire content on every token. Track where the last clean wrap boundary was and only recompute from there. For a 50k token conversation this is the difference between O(n) and O(1) per token.

conversation.rs — virtual scrolling is non-negotiable
rust
pub struct ConversationView {
    messages: Vec<MessageWidget>,
    /// Cached heights per message (invalidated on resize)
    heights: Vec<u16>,
    total_height: u16,
    scroll_offset: u16,
    viewport_height: u16,
}

impl ConversationView {
    fn visible_range(&self) -> Range<usize> {
        // Binary search on cumulative heights
        // Only render messages that overlap the viewport
        // This is what makes 1000-message sessions not lag
    }
}
input.rs — multiline with real keybindings
This is where most TUI frameworks phone it in. You want:

rust
pub struct Input {
    lines: Vec<String>,
    cursor: (usize, usize),  // (line, col) in grapheme units
    /// history: Vec<Vec<String>>,
    /// selection: Option<Range>,
}

// Keybindings worth implementing:
// Ctrl+A / Home — beginning of line
// Ctrl+E / End  — end of line
// Ctrl+W        — delete word back (essential for terminal users)
// Alt+B/F       — word nav
// Ctrl+U        — clear to beginning
// Ctrl+K        — kill to end (yank buffer for Ctrl+Y)
// Up/Down       — history navigation
// Shift+Enter   — newline (vs Enter = submit)
// Ctrl+Enter    — alternative submit for some users
Getting these right is what separates a coding agent input from a toy.

Dependency surface
toml
[dependencies]
crossterm = "0.28"          # terminal I/O, keep as impl detail
taffy = "0.7"               # layout — already in rnk, battle tested
tokio = { version = "1", features = ["full"] }
unicode-segmentation = "1"  # grapheme clusters
unicode-width = "0.2"       # cell widths for CJK etc
syntect = "5"               # syntax highlighting — lazy-load themes

[dev-dependencies]
# snapshot testing your widget output — extremely valuable
# write tests that render to a Buffer and compare strings
Notably absent: ratatui. You don't need it if you own the buffer and diff layer.

Testing approach
Since you own the buffer, testing is just string comparison:

rust
#[test]
fn streaming_block_renders_partial_token() {
    let mut w = StreamingText::new();
    w.push_token("Hello, ");
    w.push_token("world");

    let mut buf = Buffer::new(40, 3);
    w.render(Rect::new(0, 0, 40, 3), &mut buf);

    assert_eq!(buf.to_string_lines(), vec![
        "Hello, world                            ",
        "                                        ",
        "                                        ",
    ]);
}
This is better than ratatui's test backend because you own what to_string_lines() does and can add ANSI-aware assertions, styled-cell assertions, etc.

Practical sequencing
Don't try to build the whole framework before using it. The order that lets you stay productive on ion:

buffer.rs + terminal.rs + bare event loop — 2-3 days, replace rnk immediately
input.rs — you need this to be usable at all
streaming.rs + conversation.rs — the core ion UX
Layout with Taffy — do this once the above are working so you know your actual layout needs
code_block.rs, tool_call.rs, diff.rs — incrementally
The thing to avoid is spending 3 weeks on a perfect generic layout engine before any of it renders ion's actual UI.

We’re only using rnk for the bottom UI anyways. So it may be beneficial to make our own library that handles everything I was talking about in the initial messages

Feb 22

Recognized architectural opportunity to build comprehensive custom library
Recognized architectural opportunity to build comprehensive custom library


That's an important detail that reframes everything. If rnk is only powering the bottom input bar, then ion is probably doing what Claude Code and similar agents do — printing conversation content to stdout above, and using the TUI framework just for the interactive input layer. That's a common and pragmatic pattern but it has real limitations:

No ability to go back and update previous messages (streaming completions that appear in-place)
No virtual scrolling over history
No collapsible tool call views
Resize doesn't reflow anything above
So the gap between "what ion currently is" and "what a great coding agent TUI looks like" is essentially the full library you described initially. That makes the case for building it much stronger — you're not replacing rnk for cosmetic reasons, you're unlocking a fundamentally different and better UX.

What this changes about the design
The library isn't just "rnk but better." It's specifically designed around the streaming agent pattern:

┌─────────────────────────────────┐
│  Conversation history           │  ← Owned by the library
│  (virtual scroll, reflow,       │     Not stdout printing
│   in-place streaming updates)   │
│                                 │
│  ┌─ User ──────────────────┐    │
│  │ explain this codebase   │    │
│  └─────────────────────────┘    │
│                                 │
│  ┌─ Assistant ─────────────┐    │
│  │ ▊ streaming here...     │    │  ← In-place, not println
│  └─────────────────────────┘    │
│                                 │
│  ┌─ Tool: read_file ───────┐    │
│  │ ▶ src/main.rs  [done]   │    │  ← Collapsible, updating
│  └─────────────────────────┘    │
│                                 │
├─────────────────────────────────┤
│  > _                            │  ← Input bar (what rnk does now)
│  [↑ history] [ctrl+c quit]      │
└─────────────────────────────────┘
This is the layout that tools like Aider, Claude Code, and Cursor terminal mode are all converging on. The difference is whether the top region is a dumb scrollback or something the library owns and can manipulate.

Library scope and name
Given this context, the library has a well-defined scope. Call it something internal for now — agent-tui, stream-tui, whatever. It has two public-facing concerns:

1. The render model — owns the full terminal, both the scrolling history region and the input bar. Not a general-purpose TUI toolkit. Specifically designed for the agent conversation pattern.

2. The streaming primitives — first-class MessageHandle type that lets you push tokens into a message that's already been rendered and have it update in-place.

rust
// The entire public API could be this clean:

let mut ui = AgentUi::new()
    .inline()           // or .fullscreen()
    .build()?;

// Start a user message
ui.push_message(Role::User, "explain this codebase");

// Start an assistant message — get a handle to stream into
let mut msg = ui.begin_assistant_message();
while let Some(token) = stream.next().await {
    msg.push_token(&token);  // renders in-place
}
msg.finish();

// Tool call with live status
let mut tool = ui.begin_tool_call("read_file", "src/main.rs");
let result = read_file("src/main.rs").await?;
tool.complete(result);  // collapses to summary or stays expanded

// Input bar — blocks until user submits
let input = ui.read_input().await?;
```

That API hides everything — layout, diffing, scrolling, virtual rendering, Taffy, crossterm — behind the pattern that ion actually uses. No generic widget tree to configure.

---

## How it relates to the initial vision

The initial discussion was about a general-purpose library comparable to ratatui/rnk/bubbletea. That's a larger scope and a different project. What you're actually describing is narrower and more immediately valuable: **a purpose-built library for the streaming coding agent UX pattern**, which happens to need the same technical foundations (owned buffer, Taffy layout, proper inline mode, virtual scroll) but with a much more opinionated and ergonomic API.

You could build it in two layers:
```
ion-tui-core    ← buffer, layout, event loop, terminal — the plumbing
ion-tui         ← AgentUi, MessageHandle, ToolCallView — the opinionated layer
ion-tui-core is the general thing from the previous discussion. ion-tui is specific to the agent pattern and is what ion itself imports. If ion-tui-core turns out to be good, other people can use it independently — but that's a consequence, not the goal.

Does that framing match what you're thinking, or do you want it to be more general from the start?

I’d prefer to make it a general purpose one eventually so I can be worked on separately and support other projects

Feb 22

Architected decoupled library design with ion as validation mechanism
Architected decoupled library design with ion as validation mechanism



That's the right call strategically. Ion becomes the first real consumer and the forcing function that prevents you from over-engineering abstractions nobody actually needs. But the library is designed from day one to have no knowledge of ion.

The practical implication is: develop them as separate crates in a workspace from the start, even if they live in the same repo temporarily.

workspace/
├── Cargo.toml          ← workspace root
├── crates/
│   └── tui/            ← the general library, no ion concepts
│       ├── src/
│       └── Cargo.toml
└── ion/                ← consumes tui as a path dep initially
    ├── src/
    └── Cargo.toml
When the library is ready to be its own repo, it lifts out cleanly. Ion just changes from a path dependency to a crates.io version. No refactoring.

What "general purpose" actually requires differently
The core technical work is the same as we discussed — buffer, diff, Taffy layout, event loop, widget trait. What changes is the API surface and what you're willing to hardcode.

You can't hardcode the event loop shape. Ion's loop handles LLM token streams. Someone building a file manager has file system events. Someone building a log viewer has a log tail. So the event loop needs to be bring-your-own:

rust
// Library provides the scaffolding, user provides the logic
pub trait App {
    type Message: Send + 'static;

    fn update(&mut self, msg: Self::Message) -> Effect<Self::Message>;
    fn view(&self) -> Element;
    fn handle_event(&self, event: Event) -> Option<Self::Message>;
}

// Effect covers the things apps typically need to do
pub enum Effect<Msg> {
    None,
    Quit,
    Batch(Vec<Effect<Msg>>),
    Command(Pin<Box<dyn Future<Output = Msg> + Send>>),
}
This is closer to bubbletea's model than hooks — update is pure, side effects go through Effect. It's more testable and composable than the React hooks approach rnk uses, and scales better to ion's complexity.

Widgets need to be composable without knowing about each other. The Element type is the key — it's what view() returns, it's what widget authors produce, and it's what the layout engine consumes:

rust
// An Element is a node in the render tree
pub struct Element {
    pub(crate) inner: Box<dyn AnyWidget>,
    pub(crate) style: LayoutStyle,   // Taffy constraints
    pub(crate) children: Vec<Element>,
}

// Widget authors implement this
pub trait Widget {
    type State: Default + 'static;
    
    fn render(state: &Self::State, area: Rect, buf: &mut Buffer);
    fn handle_event(state: &mut Self::State, event: &Event) -> Option<WidgetMsg>;
}

// The Into<Element> blanket impl is what makes ergonomic composition work
```

**The built-in widget set should be minimal but complete.** Don't ship a chart widget you haven't needed. Ship the primitives that everything else is built from:

- `Text` — styled, wrapped
- `Block` — border, title, padding
- `List` — virtualized, arbitrary item heights  
- `Input` — single and multiline
- `Scroll` — wraps any element
- `Row` / `Col` — flex containers
- `Overlay` — for modals, dropdowns
- `Canvas` — escape hatch for custom rendering

Ion then builds `StreamingText`, `ConversationView`, `ToolCallView`, `CodeBlock`, `DiffView` on top of these primitives. Those live in ion, not the library — unless they turn out to be generic enough to contribute back.

---

## Naming and identity

Worth thinking about early because it affects motivation and how you talk about it. The library needs a name that isn't ion-adjacent. Something short, available on crates.io, that conveys "terminal UI":

A few directions — `verdigris`, `ansi-forge`, `termframe`, `pane`, `tui2` (boring but clear). Worth checking crates.io availability before you get attached. The name also affects whether people find it and whether it signals ambition.

---

## Realistic sequencing given dual goals

The risk with "general purpose from the start" is you spend months on the framework and ion stalls. The way to avoid that:

**Phase 1 — Ion drives the design (now → ion is feature complete)**
Build the library features that ion needs, in the library. No ion-specific code in the library. Ion is the only consumer so you'll discover API pain immediately. Expect the API to change a lot.

**Phase 2 — Stabilize and extract (once ion works end-to-end)**
Write docs. Write examples that aren't ion. Write the things a second consumer would need. Stabilize the widget trait. Publish to crates.io.

**Phase 3 — Second consumer**
Build something small and different with it — a file picker, a log viewer, anything. This is where you find everything that was accidentally coupled to ion's usage patterns. Fix it. Now you actually have a general library.

The mistake to avoid is designing for Phase 3 users during Phase 1. You don't know what they need yet. Ion does.

---

## What to actually build first

Given all of that, the order that keeps ion moving while establishing the library correctly:
```
Week 1-2:  buffer + terminal (setup/teardown, inline + fullscreen, resize)
Week 2-3:  event loop skeleton + App trait
Week 3-4:  Text + Block + Row/Col + Taffy integration
Week 4-5:  Input (multiline, real keybindings)
Week 5-6:  List with virtual scroll
Then:      ion builds StreamingText, ConversationView, ToolCallView on top
At the end of week 6 you have a real general-purpose library and ion has the foundation to build its actual UX. Want to start scoping out the buffer and terminal layer in detail?

Build a design spec, detailed enough for me to bring to claude code

Feb 22

Decided to generate comprehensive markdown specification document
Decided to generate comprehensive markdown specification document



The spec covers everything Claude Code needs to start implementing without guessing:

All types are defined with exact signatures and fields
Buffer diff, unicode handling, and inline mode specifics are called out in detail
The App + Effect architecture is fully specified so the event loop can be built correctly first try
Phase ordering keeps ion unblocked — you get a working input bar after Phase 4, and the conversation view foundation after Phase 5
The open questions section flags the two or three decisions that need to be made before code is written to avoid rework
A few things worth deciding before you hand this off: the crate name (check crates.io now), and whether you want RenderContext from the start or just bare (area, buf) — the spec recommends context but notes it's your call.

Tui design spec
Document · MD 




Claude is AI and can make mistakes. Please double-check responses.
Could not connect to MCP server mcp-registry
Could not connect to MCP server Claude in Chrome
