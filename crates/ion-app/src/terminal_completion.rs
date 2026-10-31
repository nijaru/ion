//! Non-modal command and file suggestions over the unsent editor.
use crate::display_text::{fit_line, fit_line_with_status};
use crate::terminal_client::paint::{Prepared, Visibility};
use crate::terminal_commands::{Builtin, COMMANDS};
use ion_host::{CommandCatalog, Resources};
use std::ops::Range;

mod files;
pub(crate) use files::FileScope;

#[derive(PartialEq, Eq)]
struct Suggestion {
    name: String,
    description: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Commands,
    Files,
}

struct Menu {
    kind: Kind,
    loading: bool,
    prefix: String,
    range: Range<usize>,
    items: Vec<Suggestion>,
    selected: usize,
}

#[derive(Default)]
pub(crate) struct Completion {
    menu: Option<Menu>,
    visibility: Visibility<CompletionCandidate>,
    dismissed: Option<(Kind, String, Range<usize>)>,
    // Explicit Tab keeps bare-token discovery active through edits in that token.
    forced_start: Option<usize>,
}

impl Completion {
    pub(crate) fn clear(&mut self) {
        self.menu = None;
        self.visibility.invalidate();
        self.dismissed = None;
        self.forced_start = None;
    }

    pub(crate) fn edited(&mut self, at: usize) {
        self.menu = None;
        self.visibility.invalidate();
        self.dismissed = None;
        if self.forced_start.is_some_and(|start| at < start) {
            self.forced_start = None;
        }
    }

    fn file_prefix(&self, draft: &str, cursor: usize, force: bool) -> Option<files::Prefix> {
        // Builtin arguments have command semantics, not ordinary prompt tokens.
        // Even explicit Tab must not replace a model identity or name with a path.
        if draft
            .split_whitespace()
            .next()
            .and_then(Builtin::parse)
            .is_some_and(|command| !matches!(command, Builtin::Image | Builtin::Export))
        {
            return None;
        }
        if let Some(token) = files::path_prefix(draft, cursor, force) {
            return Some(token);
        }
        let start = self.forced_start?;
        files::path_prefix(draft, cursor, true).filter(|token| token.range.start == start)
    }

    pub(crate) fn file_scope(
        &self,
        draft: &str,
        cursor: usize,
        cwd: &std::path::Path,
        home: Option<&std::path::Path>,
        force: bool,
    ) -> Option<FileScope> {
        self.file_prefix(draft, cursor, force)
            .map(|token| token.scope(cwd, home))
    }

    pub(crate) fn refresh(
        &mut self,
        draft: &str,
        cursor: usize,
        resources: Option<&Resources>,
        commands: Option<&CommandCatalog>,
        paths: Option<&[String]>,
        force: bool,
    ) {
        if let Some(token) = self.file_prefix(draft, cursor, force) {
            if force {
                self.forced_start = Some(token.range.start);
            } else if self.forced_start != Some(token.range.start) {
                self.forced_start = None;
            }
            let prefix = &draft[token.range.start..cursor];
            // Discovery publishes one immutable path snapshot per episode.
            // Reuse filtered rows for draws/resizes and control updates; only
            // actual editor changes or the loading transition need filtering.
            if self.menu.as_ref().is_some_and(|menu| {
                menu.kind == Kind::Files
                    && menu.prefix == prefix
                    && menu.range == token.range
                    && menu.loading == paths.is_none()
            }) {
                return;
            }
            self.set_menu(
                Kind::Files,
                prefix,
                token.range.clone(),
                paths
                    .map(|paths| token.suggestions(paths))
                    .unwrap_or_default(),
                paths.is_none(),
                force,
            );
            return;
        }
        self.forced_start = None;
        let start = draft.len() - draft.trim_start().len();
        let end = draft[start..]
            .find(char::is_whitespace)
            .map_or(draft.len(), |at| start + at);
        if cursor <= start || cursor > end || !draft[start..].starts_with('/') {
            self.clear();
            return;
        }
        let prefix = &draft[start + 1..cursor];
        if self.dismissed.as_ref().is_some_and(|(kind, text, range)| {
            *kind == Kind::Commands && text == prefix && *range == (start..end)
        }) && !force
        {
            self.menu = None;
            return;
        }
        self.dismissed = None;
        let matches = |name: &str| {
            name.starts_with(prefix)
                || name
                    .strip_prefix("skill:")
                    .is_some_and(|name| name.starts_with(prefix))
        };
        let mut items = COMMANDS
            .iter()
            .filter(|command| matches(command.name))
            .map(|command| Suggestion {
                name: command.name.into(),
                description: format!(
                    "{}{}{}",
                    command.hint,
                    if command.hint.is_empty() { "" } else { " · " },
                    command.description
                ),
            })
            .collect::<Vec<_>>();
        if let Some(resources) = resources {
            items.extend(
                resources
                    .templates()
                    .filter(|template| {
                        matches(&template.name)
                            && Builtin::parse(&format!("/{}", template.name)).is_none()
                    })
                    .map(|template| Suggestion {
                        name: template.name.clone(),
                        description: format!(
                            "[prompt] {}{}{}",
                            template.argument_hint.as_deref().unwrap_or(""),
                            if template.argument_hint.is_some() {
                                " · "
                            } else {
                                ""
                            },
                            template.description
                        ),
                    }),
            );
            items.extend(
                resources
                    .skills()
                    .filter(|skill| matches(&format!("skill:{}", skill.name)))
                    .map(|skill| Suggestion {
                        name: format!("skill:{}", skill.name),
                        description: format!("[skill] {}", skill.description),
                    }),
            );
        }
        if let Some(commands) = commands {
            items.extend(
                commands
                    .iter()
                    .filter(|command| {
                        matches(&format!("command:{}", command.name))
                            && Builtin::parse(&format!("/command:{}", command.name)).is_none()
                    })
                    .map(|command| Suggestion {
                        name: format!("command:{}", command.name),
                        description: format!(
                            "[command · {}] {}",
                            command.origin, command.description
                        ),
                    }),
            );
        }
        items.sort_by(|a, b| a.name.cmp(&b.name));
        if items.is_empty()
            || (!force
                && !self.menu.as_ref().is_some_and(|menu| {
                    menu.kind == Kind::Commands
                        && menu.prefix == prefix
                        && menu.range == (start..end)
                })
                && cursor == end
                && items.iter().any(|item| item.name == prefix))
        {
            self.menu = None;
            return;
        }
        self.set_menu(Kind::Commands, prefix, start..end, items, false, force);
    }

    fn set_menu(
        &mut self,
        kind: Kind,
        prefix: &str,
        range: Range<usize>,
        items: Vec<Suggestion>,
        loading: bool,
        force: bool,
    ) {
        if !force
            && self
                .dismissed
                .as_ref()
                .is_some_and(|(old_kind, text, old_range)| {
                    *old_kind == kind && text == prefix && *old_range == range
                })
        {
            self.menu = None;
            return;
        }
        self.dismissed = None;
        let previous = self
            .menu
            .as_ref()
            .filter(|menu| menu.kind == kind && menu.prefix == prefix && menu.range == range);
        if previous.is_some_and(|menu| menu.loading == loading && menu.items == items) {
            return;
        }
        self.visibility.invalidate();

        let selected_name = previous
            .and_then(|menu| menu.items.get(menu.selected))
            .map(|item| &item.name);
        let selected = selected_name
            .and_then(|name| items.iter().position(|item| &item.name == name))
            .unwrap_or(0);
        self.menu = Some(Menu {
            kind,
            loading,
            prefix: prefix.into(),
            range,
            items,
            selected,
        });
    }

    pub(crate) fn wants_files(&self) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| menu.kind == Kind::Files)
    }

    pub(crate) fn active(&self) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| menu.loading || !menu.items.is_empty())
    }

    pub(crate) fn invalidate_visibility(&mut self) {
        self.visibility.invalidate();
    }

    pub(crate) fn visible(&self) -> bool {
        self.menu.as_ref().is_some_and(|menu| {
            menu.items.get(menu.selected).is_some_and(|item| {
                self.visibility
                    .painted()
                    .is_some_and(|painted| *painted == CompletionCandidate::of(item))
            })
        })
    }

    pub(crate) fn move_selection(&mut self, down: bool) {
        if let Some(menu) = &mut self.menu
            && !menu.items.is_empty()
        {
            self.visibility.invalidate();
            menu.selected = if down {
                (menu.selected + 1).min(menu.items.len() - 1)
            } else {
                menu.selected.saturating_sub(1)
            };
        }
    }

    pub(crate) fn dismiss(&mut self) {
        self.visibility.invalidate();
        self.forced_start = None;
        if let Some(menu) = self.menu.take() {
            self.dismissed = Some((menu.kind, menu.prefix, menu.range));
        }
    }

    pub(crate) fn replacement(&self, draft: &str) -> Option<(Range<usize>, String, usize)> {
        if !self.visible() {
            return None;
        }
        let menu = self.menu.as_ref()?;
        let name = &menu.items.get(menu.selected)?.name;
        if menu.kind == Kind::Files {
            return Some(files::replacement(draft, &menu.range, name));
        }
        let following_space = draft[menu.range.end..]
            .chars()
            .next()
            .filter(|ch| ch.is_whitespace());
        let text = format!(
            "/{name}{}",
            if following_space.is_some() { "" } else { " " }
        );
        let cursor = menu.range.start + text.len() + following_space.map_or(0, char::len_utf8);
        Some((menu.range.clone(), text, cursor))
    }

    pub(crate) fn rows(
        &mut self,
        width: usize,
        budget: usize,
        keys: &crate::keybindings::KeyBindings,
    ) -> CompletionFrame {
        use crate::keybindings::KeyAction;
        self.visibility.invalidate();
        let Some(menu) = &mut self.menu else {
            return CompletionFrame::default();
        };
        if menu.items.is_empty() {
            return CompletionFrame {
                candidate: None,
                rows: if menu.loading && budget > 0 {
                    vec![fit_line(
                        &format!(
                            "Files · discovering paths… · {} close",
                            keys.hint(KeyAction::Cancel)
                        ),
                        width,
                    )]
                } else {
                    Vec::new()
                },
            };
        }
        if budget < 2 {
            return CompletionFrame::default();
        }
        let visible = budget.saturating_sub(1).min(4);
        let start = menu.selected.saturating_sub(visible.saturating_sub(1));
        let accept = if menu.kind == Kind::Files {
            format!(
                "{} / {}",
                keys.hint(KeyAction::Tab),
                keys.hint(KeyAction::Confirm)
            )
        } else {
            keys.hint(KeyAction::Tab)
        };
        let title = if menu.kind == Kind::Files {
            "Files"
        } else {
            "Commands"
        };
        let mut rows = vec![fit_line(
            &format!(
                "{title} · {} / {} select · {accept} complete · {} close",
                keys.hint(KeyAction::SelectUp),
                keys.hint(KeyAction::SelectDown),
                keys.hint(KeyAction::Cancel)
            ),
            width,
        )];
        let mut candidate = None;
        for (index, item) in menu.items.iter().enumerate().skip(start).take(visible) {
            let marker = if index == menu.selected { '›' } else { ' ' };
            if menu.kind == Kind::Files {
                let (label, omitted) = fit_line_with_status(&item.name, width.saturating_sub(2));
                if index == menu.selected
                    && width > 2
                    && !label.is_empty()
                    && !(omitted && label == "…")
                {
                    candidate = Some(self.visibility.prepare(CompletionCandidate::of(item)));
                }
                rows.push(fit_line(&format!("{marker} {label}"), width));
            } else {
                let (label, omitted) = fit_line_with_status(&item.name, width.saturating_sub(3));
                if index == menu.selected
                    && width > 3
                    && !label.is_empty()
                    && !(omitted && label == "…")
                {
                    candidate = Some(self.visibility.prepare(CompletionCandidate::of(item)));
                }
                rows.push(fit_line(
                    &format!("{marker} /{} — {}", item.name, item.description),
                    width,
                ));
            }
        }
        CompletionFrame { rows, candidate }
    }

    pub(crate) fn observe_paint(&mut self, frame: CompletionFrame) {
        if let Some(candidate) = frame.candidate {
            self.visibility.observe(candidate);
        } else {
            self.visibility.invalidate();
        }
    }

    #[cfg(test)]
    pub(crate) fn painted_rows(
        &mut self,
        width: usize,
        budget: usize,
        keys: &crate::keybindings::KeyBindings,
    ) -> Vec<String> {
        let frame = self.rows(width, budget, keys);
        let rows = frame.rows.clone();
        self.observe_paint(frame);
        rows
    }
}

#[derive(Default)]
pub(crate) struct CompletionFrame {
    pub rows: Vec<String>,
    candidate: Option<Prepared<CompletionCandidate>>,
}

/// Identity of the retained typed suggestion in its frozen menu. Compared only,
/// never dereferenced; editing/replacement revokes the menu's visibility episode.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CompletionCandidate(*const Suggestion);

impl CompletionCandidate {
    fn of(item: &Suggestion) -> Self {
        Self(std::ptr::from_ref(item))
    }
}
