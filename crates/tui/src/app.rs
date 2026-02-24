use std::{future::Future, io, pin::Pin, time::Duration};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture,
    },
    execute,
};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::{
    buffer::Buffer,
    error::Result,
    event::{Event, KeyCode, KeyModifiers, translate_event},
    geometry::Position,
    layout::compute_layout,
    terminal::{RenderMode, Terminal},
    widgets::Element,
};

// Minimum inline height — never drop below 1 row even if the buffer is blank.
const MIN_INLINE_HEIGHT: u16 = 1;

// ── App trait ────────────────────────────────────────────────────────────────

/// The trait users implement to build an application.
///
/// Follows a message-passing architecture (Elm / Bubbletea style). State
/// mutation only happens in `update`; `view` is pure. `handle_event` maps
/// terminal events to app messages.
pub trait App: Sized + Send + 'static {
    /// The message type for this app.
    type Message: Send + 'static;

    /// Handle a message and return an effect.
    /// Called exclusively in the event loop — must not block.
    fn update(&mut self, msg: Self::Message) -> Effect<Self::Message>;

    /// Produce the current UI tree. Called after every state change.
    ///
    /// Takes `&mut self` to allow widgets to update cached derived state
    /// (e.g., line-height caches that depend on the current terminal width).
    /// Avoid primary-state mutation here — use `update` for that.
    fn view(&mut self) -> Element;

    /// Translate a raw terminal event into an app message.
    /// Return `None` to let the framework handle the event (Ctrl+C, resize).
    fn handle_event(&self, event: &Event) -> Option<Self::Message>;

    /// Rate at which [`Event::Tick`] is sent. `None` (default) disables ticks.
    fn tick_rate(&self) -> Option<Duration> {
        None
    }

    /// Called once before the event loop starts.
    /// Return an `Effect` to kick off initial commands.
    fn init(&mut self) -> Effect<Self::Message> {
        Effect::None
    }

    /// Called after the event loop ends (after terminal is restored).
    fn on_exit(&mut self) {}

    /// Return the desired hardware cursor position (col, row) in buffer-local
    /// coordinates. The framework positions the terminal cursor here after each
    /// render. Return `None` to hide the cursor.
    fn cursor_position(&self) -> Option<(u16, u16)> {
        None
    }

    /// Lines to insert above the inline region before rendering.
    ///
    /// Called before each render frame. Return empty vec for no insertion.
    /// Used in inline mode to push chat content into native terminal scrollback.
    fn pre_render_insert(&mut self) -> Vec<String> {
        Vec::new()
    }
}

// ── Effect ───────────────────────────────────────────────────────────────────

/// Effects are the mechanism for side effects and async work.
/// Returned from `App::update` and executed by the runtime.
pub enum Effect<Msg> {
    /// No side effect.
    None,
    /// Signal the event loop to exit cleanly.
    Quit,
    /// Run multiple effects.
    Batch(Vec<Effect<Msg>>),
    /// Run an async task; the result is sent back as a message.
    Command(Pin<Box<dyn Future<Output = Msg> + Send>>),
    /// Emit a message immediately on the next update tick.
    Emit(Msg),
}

impl<Msg: Send + 'static> Effect<Msg> {
    /// Wrap a future as a `Command` effect.
    pub fn command<F>(f: F) -> Self
    where
        F: Future<Output = Msg> + Send + 'static,
    {
        Effect::Command(Box::pin(f))
    }

    /// Batch multiple effects into one.
    pub fn batch(effects: impl IntoIterator<Item = Effect<Msg>>) -> Self {
        Effect::Batch(effects.into_iter().collect())
    }
}

// ── AppBuilder ───────────────────────────────────────────────────────────────

/// Configures and launches an [`App`].
///
/// ```no_run
/// # async fn run() -> tui::error::Result<()> {
/// # use tui::{app::{App, AppBuilder, Effect}, event::Event, widgets::{Element, IntoElement, canvas::Canvas}};
/// # struct MyApp;
/// # impl App for MyApp {
/// #     type Message = ();
/// #     fn update(&mut self, _: ()) -> Effect<()> { Effect::None }
/// #     fn view(&mut self) -> Element { Canvas::new(|_, _| {}).into_element() }
/// #     fn handle_event(&self, _: &Event) -> Option<()> { None }
/// # }
/// AppBuilder::new(MyApp).inline(3).run().await?;
/// # Ok(())
/// # }
/// ```
pub struct AppBuilder<A: App> {
    app: A,
    mode: RenderMode,
    mouse_capture: bool,
    focus_events: bool,
    bracketed_paste: bool,
    msg_tx: mpsc::UnboundedSender<A::Message>,
    msg_rx: mpsc::UnboundedReceiver<A::Message>,
}

impl<A: App> AppBuilder<A> {
    pub fn new(app: A) -> Self {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        Self {
            app,
            mode: RenderMode::Fullscreen,
            mouse_capture: false,
            focus_events: false,
            bracketed_paste: false,
            msg_tx,
            msg_rx,
        }
    }

    /// Create an `AppBuilder` with a pre-created channel.
    ///
    /// Use this when the app itself needs to hold a clone of `msg_tx` before
    /// `run()` is called (e.g., to spawn agent tasks that push messages into
    /// the event loop). The caller owns the channel lifetime.
    pub fn new_with_channel(
        app: A,
        msg_tx: mpsc::UnboundedSender<A::Message>,
        msg_rx: mpsc::UnboundedReceiver<A::Message>,
    ) -> Self {
        Self {
            app,
            mode: RenderMode::Fullscreen,
            mouse_capture: false,
            focus_events: false,
            bracketed_paste: false,
            msg_tx,
            msg_rx,
        }
    }

    /// Return a sender that can push messages into the app from external tasks.
    ///
    /// The sender is unbounded and can be cloned freely. Call before `run()`.
    pub fn message_sender(&self) -> mpsc::UnboundedSender<A::Message> {
        self.msg_tx.clone()
    }

    pub fn inline(mut self, height: u16) -> Self {
        self.mode = RenderMode::Inline { height };
        self
    }

    pub fn fullscreen(mut self) -> Self {
        self.mode = RenderMode::Fullscreen;
        self
    }

    pub fn mouse(mut self, enabled: bool) -> Self {
        self.mouse_capture = enabled;
        self
    }

    pub fn focus_events(mut self, enabled: bool) -> Self {
        self.focus_events = enabled;
        self
    }

    pub fn bracketed_paste(mut self, enabled: bool) -> Self {
        self.bracketed_paste = enabled;
        self
    }

    /// Start the event loop. Blocks until the app exits.
    /// Returns the app (with final state) on success.
    pub async fn run(self) -> Result<A> {
        let mut out = io::stdout();

        // Enable optional terminal features before entering raw mode handling.
        if self.mouse_capture {
            execute!(out, EnableMouseCapture)?;
        }
        if self.focus_events {
            execute!(out, EnableFocusChange)?;
        }
        if self.bracketed_paste {
            execute!(out, EnableBracketedPaste)?;
        }

        let terminal = Terminal::new(self.mode)?;
        let area = terminal.render_area();

        let runner = AppRunner {
            app: self.app,
            terminal,
            prev_buf: Buffer::empty(area),
            msg_tx: self.msg_tx,
            msg_rx: self.msg_rx,
            dirty: true,
            mouse_capture: self.mouse_capture,
            focus_events: self.focus_events,
            bracketed_paste: self.bracketed_paste,
        };

        runner.run_loop().await
    }
}

// ── AppRunner (internal) ─────────────────────────────────────────────────────

struct AppRunner<A: App> {
    app: A,
    terminal: Terminal,
    prev_buf: Buffer,
    msg_tx: mpsc::UnboundedSender<A::Message>,
    msg_rx: mpsc::UnboundedReceiver<A::Message>,
    dirty: bool,
    mouse_capture: bool,
    focus_events: bool,
    bracketed_paste: bool,
}

impl<A: App> AppRunner<A> {
    async fn run_loop(mut self) -> Result<A> {
        // Run init effect.
        let init = self.app.init();
        self.execute_effect(init).await;

        // Initial render.
        self.render()?;
        self.dirty = false;

        let mut event_stream = crossterm::event::EventStream::new();
        let mut tick_interval = self.app.tick_rate().map(tokio::time::interval);
        let mut render_interval = tokio::time::interval(Duration::from_millis(16)); // 60fps ceiling
        render_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // App messages (from commands, external senders).
                Some(msg) = self.msg_rx.recv() => {
                    let effect = self.app.update(msg);
                    let quit = matches!(effect, Effect::Quit);
                    self.execute_effect(effect).await;
                    self.dirty = true;
                    if quit { break; }
                }

                // Terminal events.
                Some(Ok(ev)) = event_stream.next() => {
                    if let Some(ev) = translate_event(ev) {
                        // Framework-level resize: update cached terminal size.
                        if let Event::Resize(w, h) = &ev {
                            self.terminal.handle_resize(*w, *h);
                            self.dirty = true;
                        }

                        // Offer event to the app first.
                        let handled = if let Some(msg) = self.app.handle_event(&ev) {
                            let effect = self.app.update(msg);
                            let quit = matches!(effect, Effect::Quit);
                            self.execute_effect(effect).await;
                            self.dirty = true;
                            if quit { break; }
                            true
                        } else {
                            false
                        };

                        // Default Ctrl+C handler if app didn't consume it.
                        if !handled {
                            if let Event::Key(k) = &ev {
                                if k.code == KeyCode::Char('c')
                                    && k.modifiers.contains(KeyModifiers::CTRL)
                                {
                                    break;
                                }
                            }
                        }
                    }
                }

                // Tick events (disabled when tick_rate returns None).
                Some(_) = tick(tick_interval.as_mut()) => {
                    if let Some(msg) = self.app.handle_event(&Event::Tick) {
                        let effect = self.app.update(msg);
                        let quit = matches!(effect, Effect::Quit);
                        self.execute_effect(effect).await;
                        self.dirty = true;
                        if quit { break; }
                    }
                }

                // Render tick — only redraws if state changed.
                _ = render_interval.tick() => {
                    if self.dirty {
                        self.render()?;
                        self.dirty = false;
                    }
                }
            }
        }

        // Disable optional features before restoring raw mode.
        let mut out = io::stdout();
        if self.mouse_capture {
            let _ = execute!(out, DisableMouseCapture);
        }
        if self.focus_events {
            let _ = execute!(out, DisableFocusChange);
        }
        if self.bracketed_paste {
            let _ = execute!(out, DisableBracketedPaste);
        }

        self.terminal.restore()?;
        self.app.on_exit();
        Ok(self.app)
    }

    fn render(&mut self) -> Result<()> {
        // Insert scrollback lines before the inline region (no-op in fullscreen).
        let insert_lines = self.app.pre_render_insert();
        if !insert_lines.is_empty() {
            self.terminal.insert_before(&insert_lines)?;
        }

        let area = self.terminal.render_area();
        let size = crate::geometry::Size {
            width: area.width,
            height: area.height,
        };
        let mut buf = Buffer::new(area);

        let root = self.app.view();
        let layout = compute_layout(&root, size);
        root.render(&layout, &mut buf);

        // In inline mode, use the actual rendered content height rather than
        // the fixed buffer height. This lets Terminal::flush_commands clear
        // rows that were occupied in the previous frame but are now empty,
        // preventing ghost lines when a multiline input shrinks.
        let rendered_height = if matches!(self.terminal.mode(), RenderMode::Inline { .. }) {
            buf.content_height().max(MIN_INLINE_HEIGHT)
        } else {
            area.height
        };

        let commands = buf.diff(&self.prev_buf);
        self.terminal.flush_commands(commands, rendered_height)?;
        self.prev_buf = buf;

        // Position the hardware cursor after rendering.
        if let Some((col, row)) = self.app.cursor_position() {
            self.terminal
                .set_cursor_position(Position { x: col, y: row })?;
            self.terminal.set_cursor_visible(true)?;
        } else {
            self.terminal.set_cursor_visible(false)?;
        }

        Ok(())
    }

    async fn execute_effect(&self, effect: Effect<A::Message>) {
        match effect {
            Effect::None => {}
            Effect::Quit => {} // handled by caller checking the return value
            Effect::Emit(msg) => {
                let _ = self.msg_tx.send(msg);
            }
            Effect::Command(fut) => {
                let tx = self.msg_tx.clone();
                tokio::spawn(async move {
                    let msg = fut.await;
                    let _ = tx.send(msg);
                });
            }
            Effect::Batch(effects) => {
                for e in effects {
                    // Recursive Box::pin to avoid infinite async recursion.
                    Box::pin(self.execute_effect(e)).await;
                }
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Returns the next tick instant, or stays pending forever if there's no
/// interval. Used in `select!` to conditionally enable the tick arm.
async fn tick(interval: Option<&mut tokio::time::Interval>) -> Option<tokio::time::Instant> {
    match interval {
        Some(i) => Some(i.tick().await),
        None => std::future::pending().await,
    }
}
