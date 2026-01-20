use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};

pub struct ChatMessage<'a> {
    pub sender: &'a str,
    pub content: &'a str,
    pub color: Color,
    pub focused: bool,
}

impl<'a> ChatMessage<'a> {
    pub fn new(sender: &'a str, content: &'a str, color: Color) -> Self {
        Self {
            sender,
            content,
            color,
            focused: false,
        }
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
}

impl Widget for ChatMessage<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let border_color = if self.focused {
            Color::Yellow
        } else {
            Color::DarkGray
        };

        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            )
            .padding(Padding::horizontal(1));

        let sender_style = Style::default().fg(self.color).bold();
        let content_style = Style::default();

        let mut lines = Vec::new();
        lines.push(Line::from(vec![Span::styled(
            format!("{}: ", self.sender),
            sender_style,
        )]));

        // Basic markdown-ish rendering for the content
        // In a real impl, we'd use tui-markdown here, but for the widget pattern
        // we'll keep it simple or assume pre-rendered lines.
        for line in self.content.lines() {
            lines.push(Line::from(Span::styled(line, content_style)));
        }

        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

pub struct LoadingIndicator {
    pub label: String,
    pub frame_count: u64,
}

impl Widget for LoadingIndicator {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let symbol = spinner[(self.frame_count % spinner.len() as u64) as usize];

        let text = format!("{} {}", symbol, self.label);
        Paragraph::new(text)
            .style(Style::default().fg(Color::Yellow))
            .render(area, buf);
    }
}
