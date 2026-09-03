use super::{Action, KeyMap, Palette};
use ratatui::text::Line;

/// Grouped binding reference for `/hotkeys` (pi parity: Navigation /
/// Editing / Other), rendered from the resolved keymap so settings
/// overrides appear as bound. `label()` joins every binding for an
/// action with `/`, matching pi's display of rebound keys.
pub(super) fn hotkeys_reference_lines(keymap: &KeyMap) -> Vec<String> {
    let l = |action: Action| keymap.label(action);
    vec![
        "Navigation".to_owned(),
        format!(
            "  {} / {} / {} / {}  move cursor · browse history",
            l(Action::HistoryPrevious),
            l(Action::HistoryNext),
            l(Action::CursorLeft),
            l(Action::CursorRight)
        ),
        format!(
            "  {} / {}  move by word · {}/{}  start/end of line",
            l(Action::CursorWordLeft),
            l(Action::CursorWordRight),
            l(Action::CursorHome),
            l(Action::CursorEnd)
        ),
        "Editing".to_owned(),
        format!(
            "  {}  send · {}  newline",
            l(Action::Submit),
            l(Action::InsertNewline)
        ),
        format!(
            "  {}  kill word back · {}  kill word forward",
            l(Action::KillWord),
            l(Action::KillWordForward)
        ),
        format!(
            "  {}/{}  kill to start/end of line",
            l(Action::KillToStart),
            l(Action::KillToEnd)
        ),
        format!(
            "  {}  yank · {}  cycle the kill ring · {}  undo",
            l(Action::Yank),
            l(Action::YankPop),
            l(Action::Undo)
        ),
        "Other".to_owned(),
        format!(
            "  {}  complete · {}  steer the running turn",
            l(Action::Complete),
            l(Action::SteerCurrent)
        ),
        format!(
            "  {}  cancel/abort · {}  clear · {}  exit (empty)",
            l(Action::Cancel),
            l(Action::ClearComposer),
            l(Action::Quit)
        ),
        format!(
            "  {}  queue follow-up · {}  restore queued",
            l(Action::QueueFollowUp),
            l(Action::DequeueFollowUp)
        ),
        format!(
            "  {}  external editor · {}  paste · {}  copy last reply",
            l(Action::ExternalEditor),
            l(Action::PasteClipboard),
            l(Action::CopyLastMessage)
        ),
        format!(
            "  {}  tools · {}  thinking · {}  cycle thinking · {}  model picker · {}/{}  cycle models",
            l(Action::ToggleToolOutput),
            l(Action::ToggleThinking),
            l(Action::CycleThinking),
            l(Action::OpenModelSelector),
            l(Action::CycleModelForward),
            l(Action::CycleModelBackward)
        ),
        "  /  slash commands · @ file reference · !/! shell".to_owned(),
    ]
}

/// Compact local discovery view for the idle composer. This is intentionally
/// smaller than `/help`: it names the high-frequency actions without filling
/// scrollback or involving the runtime.
pub(super) fn hotkey_lines(keymap: &KeyMap, palette: &Palette) -> Vec<Line<'static>> {
    let line_style = palette.system_note;
    vec![
        Line::from(format!(
            "? show/hide  ·  esc close  ·  {} cancel  ·  /help commands",
            keymap.label(Action::Cancel)
        ))
        .style(line_style),
        Line::from(format!(
            "{} submit  ·  {} steer  ·  {} newline",
            keymap.label(Action::Submit),
            keymap.label(Action::SteerCurrent),
            keymap.label(Action::InsertNewline)
        ))
        .style(line_style),
        Line::from(format!(
            "{} complete  ·  {}/{} history  ·  {}/{} models",
            keymap.label(Action::Complete),
            keymap.label(Action::HistoryPrevious),
            keymap.label(Action::HistoryNext),
            keymap.label(Action::CycleModelForward),
            keymap.label(Action::CycleModelBackward)
        ))
        .style(line_style),
        Line::from(format!(
            "{} picker  ·  {} tools  ·  {} thinking",
            keymap.label(Action::OpenModelSelector),
            keymap.label(Action::ToggleToolOutput),
            keymap.label(Action::ToggleThinking)
        ))
        .style(line_style),
        Line::from(
            "@ file reference (fuzzy search) · ! shell passthrough · ctrl+v paste".to_owned(),
        )
        .style(line_style),
        Line::from(format!(
            "{} clear/exit  ·  {} quit",
            keymap.label(Action::ClearComposer),
            keymap.label(Action::Quit)
        ))
        .style(line_style),
    ]
}
