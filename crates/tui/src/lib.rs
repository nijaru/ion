pub mod app;
pub mod buffer;
pub mod error;
pub mod event;
pub mod geometry;
pub mod layout;
pub mod style;
pub mod terminal;
pub mod theme;
pub mod widgets;

// ── Convenience re-exports ────────────────────────────────────────────────────
// These bring the most-used types to the crate root so library users can write
// `tui::Style` instead of `tui::style::Style`.

pub use error::{Result, TuiError};
pub use geometry::{Position, Rect, Size};
pub use style::{Color, Style, StyleModifiers};
pub use theme::Theme;
pub use widgets::{Element, IntoElement};

// Widget re-exports
pub use widgets::{
    block::{Block, BorderType, TitlePosition},
    canvas::Canvas,
    col::Col,
    input::{Input, InputAction, InputState},
    list::{List, ListState},
    row::Row,
    scroll::{Scroll, ScrollDirection, ScrollState},
    text::{Alignment, Span, Text, WrapMode},
};
