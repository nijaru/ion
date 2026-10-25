//! Non-modal command and project-file suggestions over the unsent editor.
use crate::display_text::{fit_line, fit_line_with_status};
use crate::terminal_commands::{Builtin, COMMANDS};
use ion_host::{CommandCatalog, Resources};
use std::ops::Range;

mod files;

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
    drawn: bool,
}

#[derive(Default)]
pub(crate) struct Completion {
    menu: Option<Menu>,
    dismissed: Option<(Kind, String, Range<usize>)>,
}

impl Completion {
    pub(crate) fn clear(&mut self) {
        self.menu = None;
        self.dismissed = None;
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
        if let Some(token) = files::prefix(draft, cursor) {
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
                    .map(|paths| files::suggestions(paths, &token.query))
                    .unwrap_or_default(),
                paths.is_none(),
                force,
            );
            return;
        }
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
        let drawn = previous.is_some_and(|menu| {
            menu.drawn
                && menu
                    .items
                    .iter()
                    .map(|item| &item.name)
                    .eq(items.iter().map(|item| &item.name))
        });

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
            drawn,
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
        if let Some(menu) = &mut self.menu {
            menu.drawn = false;
        }
    }

    pub(crate) fn visible(&self) -> bool {
        self.menu.as_ref().is_some_and(|menu| menu.drawn)
    }

    pub(crate) fn unique(&self) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| menu.kind == Kind::Commands && menu.items.len() == 1)
    }

    pub(crate) fn move_selection(&mut self, down: bool) {
        if let Some(menu) = &mut self.menu
            && !menu.items.is_empty()
        {
            menu.drawn = false;
            menu.selected = if down {
                (menu.selected + 1).min(menu.items.len() - 1)
            } else {
                menu.selected.saturating_sub(1)
            };
        }
    }

    pub(crate) fn dismiss(&mut self) {
        if let Some(menu) = self.menu.take() {
            self.dismissed = Some((menu.kind, menu.prefix, menu.range));
        }
    }

    pub(crate) fn replacement(&self, draft: &str) -> Option<(Range<usize>, String, usize)> {
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

    pub(crate) fn rows(&mut self, width: usize, budget: usize) -> Vec<String> {
        let Some(menu) = &mut self.menu else {
            return Vec::new();
        };
        menu.drawn = false;
        if menu.items.is_empty() {
            return if menu.loading && budget > 0 {
                vec![fit_line(
                    "Files · discovering project paths… · Esc close",
                    width,
                )]
            } else {
                Vec::new()
            };
        }
        if budget < 2 {
            return Vec::new();
        }
        let visible = budget.saturating_sub(1).min(4);
        let start = menu.selected.saturating_sub(visible.saturating_sub(1));
        let mut rows = vec![fit_line(
            if menu.kind == Kind::Files {
                "Files · ↑/↓ select · Tab/Enter complete · Esc close"
            } else {
                "Commands · ↑/↓ select · Tab complete · Esc close"
            },
            width,
        )];
        for (index, item) in menu.items.iter().enumerate().skip(start).take(visible) {
            let marker = if index == menu.selected { '›' } else { ' ' };
            if menu.kind == Kind::Files {
                let (label, omitted) = fit_line_with_status(&item.name, width.saturating_sub(2));
                if index == menu.selected {
                    menu.drawn = width > 2 && !label.is_empty() && !(omitted && label == "…");
                }
                rows.push(fit_line(&format!("{marker} {label}"), width));
            } else {
                menu.drawn = true;
                rows.push(fit_line(
                    &format!("{marker} /{} — {}", item.name, item.description),
                    width,
                ));
            }
        }
        rows
    }
}
