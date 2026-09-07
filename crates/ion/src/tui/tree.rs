//! Presentation of the runtime's immutable conversation tree.

use super::*;
use std::collections::HashMap;

use ion_core::{EntryId, EntryRecord, SessionEntry};

/// Depth-first rows preserve parent/child adjacency across branch changes.
/// Iteration avoids recursion on long sessions; indentation is bounded for
/// narrow terminals, while the entry identity stays exact for navigation.
pub(super) fn rows(entries: Vec<EntryRecord>) -> Vec<(EntryId, String)> {
    let mut children: HashMap<Option<EntryId>, Vec<EntryRecord>> = HashMap::new();
    for entry in entries {
        children.entry(entry.parent).or_default().push(entry);
    }
    let mut pending: Vec<_> = children
        .remove(&None)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .map(|entry| (entry, 0usize))
        .collect();
    let mut rows = Vec::new();
    while let Some((entry, depth)) = pending.pop() {
        let label = label(&entry.entry);
        rows.push((entry.id, format!("{}{}", "  ".repeat(depth.min(8)), label)));
        for child in children
            .remove(&Some(entry.id))
            .unwrap_or_default()
            .into_iter()
            .rev()
        {
            pending.push((child, depth + 1));
        }
    }
    rows
}

fn label(entry: &SessionEntry) -> String {
    let (kind, text) = match entry {
        SessionEntry::UserMessage { text } => ("user", text.as_str()),
        SessionEntry::AssistantMessage { text } => ("assistant", text.as_str()),
        SessionEntry::ToolCall { call } => ("tool", call.name.as_str()),
        SessionEntry::ToolResult { .. } => ("tool result", ""),
        SessionEntry::Compaction { .. } => ("compaction", ""),
        SessionEntry::AgentMessage { text, .. } => ("agent", text.as_str()),
        SessionEntry::ShellExecution { command, .. } => ("shell", command.as_str()),
    };
    let first = text.lines().next().unwrap_or_default();
    let mut preview: String = first.chars().take(60).collect();
    if first.chars().count() > 60 {
        preview.push('…');
    }
    if preview.is_empty() {
        kind.to_owned()
    } else {
        format!("{kind}: {preview}")
    }
}

/// One immutable session entry with a bounded presentation label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TreeRow {
    pub(super) entry_id: ion_core::EntryId,
    pub(super) label: String,
}

/// Ephemeral /tree picker (pi parity: the tree selector): rows are
/// the whole session tree in depth-first order; the composer filters;
/// enter navigates the lane to that point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TreeSelector {
    pub(super) rows: Vec<TreeRow>,
    pub(super) selected: usize,
    pub(super) saved_composer: String,
    pub(super) saved_cursor: usize,
}

impl UiState {
    pub(super) fn open_tree_selector(
        &mut self,
        rows: Vec<TreeRow>,
        current: Option<ion_core::EntryId>,
    ) {
        if self.tree_selector.is_some() || rows.is_empty() {
            return;
        }
        let saved_composer = std::mem::take(&mut self.composer);
        let saved_cursor = self.cursor;
        self.composer.clear();
        self.cursor = 0;
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
        let selected = rows
            .iter()
            .position(|row| Some(row.entry_id) == current)
            .unwrap_or(0);
        self.tree_selector = Some(TreeSelector {
            rows,
            selected,
            saved_composer,
            saved_cursor,
        });
    }

    pub(super) fn close_tree_selector(&mut self) {
        let Some(selector) = self.tree_selector.take() else {
            return;
        };
        self.composer = selector.saved_composer;
        self.cursor = selector.saved_cursor.min(self.composer.chars().count());
        self.preferred_column = None;
        self.undo_stack.clear();
        self.last_edit = None;
    }

    pub(super) fn filtered_tree_rows(&self) -> Vec<TreeRow> {
        self.tree_selector
            .as_ref()
            .map(|selector| {
                let query = self.composer.to_lowercase();
                selector
                    .rows
                    .iter()
                    .filter(|row| fuzzy_contains(&row.label.to_lowercase(), &query))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn selected_tree_row(&self) -> Option<TreeRow> {
        let selector = self.tree_selector.as_ref()?;
        self.filtered_tree_rows().get(selector.selected).cloned()
    }

    pub(super) fn move_tree_selection(&mut self, delta: isize) {
        let count = self.filtered_tree_rows().len();
        let Some(selector) = self.tree_selector.as_mut() else {
            return;
        };
        if count == 0 {
            selector.selected = 0;
            return;
        }
        selector.selected =
            (selector.selected as isize + delta).rem_euclid(count as isize) as usize;
    }

    pub(super) fn reset_tree_selection(&mut self) {
        let selected = self
            .filtered_tree_rows()
            .iter()
            .position(|row| row.label.eq_ignore_ascii_case(&self.composer))
            .unwrap_or(0);
        if let Some(selector) = self.tree_selector.as_mut() {
            selector.selected = selected;
        }
    }
}

pub(super) fn handle_tree_selector_key(
    mut state: UiState,
    key: KeyEvent,
) -> (UiState, Option<UiEffect>) {
    match key.code {
        KeyCode::Esc if key.modifiers.is_empty() => {
            state.close_tree_selector();
            (state, None)
        }
        KeyCode::Enter if key.modifiers.is_empty() => {
            let Some(row) = state.selected_tree_row() else {
                state
                    .pending_scrollback
                    .push(Line::from("no matching entries").red());
                return (state, None);
            };
            state.close_tree_selector();
            (
                state,
                Some(UiEffect::NavigateLeaf {
                    entry_id: row.entry_id,
                }),
            )
        }
        KeyCode::Up if key.modifiers.is_empty() => {
            state.move_tree_selection(-1);
            (state, None)
        }
        KeyCode::Down if key.modifiers.is_empty() => {
            state.move_tree_selection(1);
            (state, None)
        }
        KeyCode::Backspace if key.modifiers.is_empty() => {
            let (state, _) = handle_backspace(state);
            let mut state = state;
            state.reset_tree_selection();
            (state, None)
        }
        KeyCode::Char(ch) if key.modifiers.is_empty() || key.modifiers == Modifiers::SHIFT => {
            insert_at_cursor(&mut state, &ch.to_string());
            state.reset_tree_selection();
            (state, None)
        }
        _ => (state, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_rows_keep_sibling_branches_and_preselect_current_leaf() {
        let root = EntryId::generate();
        let old = EntryId::generate();
        let alternate = EntryId::generate();
        let make = |id, parent, seq, text: &str| EntryRecord {
            id,
            parent,
            seq,
            entry: SessionEntry::UserMessage {
                text: text.to_owned(),
            },
        };
        let rows = rows(vec![
            make(root, None, 0, "root"),
            make(old, Some(root), 1, "old"),
            make(alternate, Some(root), 2, "alternate"),
        ]);
        assert_eq!(
            rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [root, old, alternate]
        );
        let mut state = UiState::new();
        state.open_tree_selector(
            rows.into_iter()
                .map(|(entry_id, label)| TreeRow { entry_id, label })
                .collect(),
            Some(old),
        );
        assert_eq!(state.selected_tree_row().unwrap().entry_id, old);
        state.move_tree_selection(1);
        let (_, effect) = handle_tree_selector_key(
            state,
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: Modifiers::NONE,
            },
        );
        assert!(
            matches!(effect, Some(UiEffect::NavigateLeaf { entry_id }) if entry_id == alternate)
        );
    }
}
