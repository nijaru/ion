//! Rendering functions for the TUI.
//!
//! Terminal APIs use u16 for dimensions; numeric casts are intentional.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

mod bottom_ui;
mod chat;
mod direct;
mod history;
pub(crate) mod layout;
pub(crate) mod popup;
pub(crate) mod selector;

use selector::MAX_VISIBLE_ITEMS;

/// Input prompt prefix " › "
pub(crate) const PROMPT: &str = " › ";
/// Continuation line prefix "   "
pub(crate) const CONTINUATION: &str = "   ";
/// Width of prompt/continuation prefix
pub(crate) const PROMPT_WIDTH: u16 = 3;
/// Total input margin (prompt + right padding)
pub(crate) const INPUT_MARGIN: u16 = 4;
/// Height of the progress bar area
pub(crate) const PROGRESS_HEIGHT: u16 = 1;
/// Selector layout overhead: tabs(1) + desc(1) + search box(3) + hint(1) + list header
pub(crate) const SELECTOR_OVERHEAD: u16 = 7;

/// Calculate selector height based on item count.
pub(crate) fn selector_height(item_count: usize, screen_height: u16) -> u16 {
    let list_height = (item_count as u16).clamp(3, MAX_VISIBLE_ITEMS);
    let needed_height = SELECTOR_OVERHEAD + list_height;
    let max_height = screen_height.saturating_sub(2);
    needed_height.min(max_height)
}
