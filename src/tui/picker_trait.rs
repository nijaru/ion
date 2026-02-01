//! Common trait for picker navigation.

/// Common navigation interface for pickers.
pub trait PickerNavigation {
    /// Move selection up by count items.
    fn move_up(&mut self, count: usize);
    /// Move selection down by count items.
    fn move_down(&mut self, count: usize);
    /// Jump to the first item.
    fn jump_to_top(&mut self);
    /// Jump to the last item.
    fn jump_to_bottom(&mut self);
}
