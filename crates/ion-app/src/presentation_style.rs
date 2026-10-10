//! Semantic presentation colors and emphasis shared by transcript renderers.
use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Copy)]
pub(crate) enum Role {
    Heading,
    Secondary,
    UserInput,
    Error,
    Warning,
    Running,
    Code,
    Link,
    Added,
    Removed,
    Hunk,
}

pub(crate) fn style(role: Role) -> Style {
    match role {
        Role::Heading => Style::default().add_modifier(Modifier::BOLD),
        // Capture notices, queued work and visible thinking must stay readable.
        // Faint is terminal-dependent and can erase contrast even when the
        // configured foreground is legible. Hierarchy comes from labels and
        // indentation instead; this is not a contrast-aware palette resolver.
        Role::Secondary => Style::default(),
        Role::UserInput => Style::default().fg(Color::Blue),
        Role::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Role::Warning => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Role::Running => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        Role::Link | Role::Hunk => Style::default().fg(Color::Cyan),
        Role::Code => Style::default().fg(Color::Magenta),
        Role::Added => Style::default().fg(Color::Green),
        Role::Removed => Style::default().fg(Color::Red),
    }
}
