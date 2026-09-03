use super::{Action, KeyMap, Palette};
use ratatui::text::Line;

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
