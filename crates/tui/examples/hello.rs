//! Hello world — Phase 2 deliverable.
//!
//! Demonstrates the App trait + Effect system. Press 'q' or Ctrl+C to quit.
//!
//! Run with: cargo run -p tui --example hello

use tui::{
    app::{App, AppBuilder, Effect},
    event::{Event, KeyCode},
    style::{Color, Style},
    widgets::{canvas::Canvas, Element, IntoElement},
};

struct Hello {
    message: &'static str,
    color: Color,
}

#[derive(Debug)]
enum Msg {
    Quit,
    ToggleColor,
}

impl App for Hello {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Effect<Msg> {
        match msg {
            Msg::Quit => Effect::Quit,
            Msg::ToggleColor => {
                self.color = if self.color == Color::Green { Color::Cyan } else { Color::Green };
                Effect::None
            }
        }
    }

    fn view(&mut self) -> Element {
        let msg = self.message;
        let color = self.color;
        Canvas::new(move |area, buf| {
            let style = Style::new().fg(color).bold();
            buf.set_string(1, 1, msg, style);
            buf.set_string(1, 2, "Press 'q' to quit, 't' to toggle color.", Style::default());
            // Draw a simple border on row 0.
            let w = area.width;
            for col in 0..w {
                buf.set_symbol(col, 0, "─", Style::new().dim());
            }
        })
        .into_element()
    }

    fn handle_event(&self, event: &Event) -> Option<Msg> {
        if let Event::Key(k) = event {
            match k.code {
                KeyCode::Char('q') | KeyCode::Esc => return Some(Msg::Quit),
                KeyCode::Char('t') => return Some(Msg::ToggleColor),
                _ => {}
            }
        }
        None
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    AppBuilder::new(Hello { message: "Hello, world!", color: Color::Green })
        .inline(4)
        .run()
        .await?;
    Ok(())
}
