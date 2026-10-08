//! Terminal command identity, discoverable resources and non-executing completion.
use std::ops::Range;

use ion_host::Resources;

use crate::display_text::fit_line;

struct Command {
    kind: Builtin,
    name: &'static str,
    hint: &'static str,
    description: &'static str,
}

// Keep dispatch identities, help and discovery in one searchable inventory.
macro_rules! commands {
    ($($kind:ident => ($name:literal, $hint:literal, $description:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum Builtin { $($kind),+ }
        const COMMANDS: &[Command] = &[
            $(Command { kind: Builtin::$kind, name: $name, hint: $hint, description: $description }),+
        ];
    };
}
commands! {
    Help => ("help", "", "Show commands and keyboard controls"),
    New => ("new", "", "Start a new Session"),
    Clone => ("clone", "", "Copy this conversation into a new Session"),
    Fork => ("fork", "[TURN]", "Fork before a Turn and restore its input"),
    ForkAfter => ("fork-after", "TURN", "Fork after a Turn"),
    Resume => ("resume", "[ID]", "Choose a saved Session"),
    Session => ("session", "", "Show Session and context information"),
    Name => ("name", "[NAME]", "Show or change the Session name"),
    Model => ("model", "[PROVIDER/MODEL]", "Choose a model"),
    Reasoning => ("reasoning", "[default|off|low|medium|high|budget:TOKENS]", "Select generation effort (route support varies)"),
    Compact => ("compact", "", "Summarize settled context"),
    Tools => ("tools", "", "List recorded tool operations"),
    Tool => ("tool", "[N]", "Inspect recorded tool evidence"),
    Settings => ("settings", "[compact|expanded|thinking show|thinking hide]", "Change terminal presentation"),
    Tui => ("tui", "[inline|fullscreen]", "Choose the transcript surface"),
    Image => ("image", "PATH", "Attach an image to the next input"),
    Copy => ("copy", "", "Copy the last completed answer"),
    Editor => ("editor", "", "Edit the draft in an external editor"),
    Export => ("export", "PATH", "Save a readable transcript"),
    Skills => ("skills", "", "List available skills"),
    Prompts => ("prompts", "", "List available prompt templates"),
    Reload => ("reload", "", "Reload project instructions and resources"),
    Login => ("login", "PROVIDER", "Save a credential using masked input"),
    Logout => ("logout", "PROVIDER", "Remove a saved credential"),
    Quit => ("quit", "", "Exit the terminal client"),
}

impl Command {
    fn usage(&self) -> String {
        format!(
            "/{}{}{}",
            self.name,
            if self.hint.is_empty() { "" } else { " " },
            self.hint
        )
    }
}

impl Builtin {
    pub(crate) fn usage(self) -> String {
        COMMANDS
            .iter()
            .find(|command| command.kind == self)
            .expect("every built-in command is registered")
            .usage()
    }
    pub(crate) fn parse(name: &str) -> Option<Self> {
        let name = name.strip_prefix('/')?;
        if name == "exit" {
            return Some(Self::Quit);
        }
        COMMANDS
            .iter()
            .find(|command| command.name == name)
            .map(|command| command.kind)
    }

    pub(crate) fn help() -> String {
        let commands = COMMANDS
            .iter()
            .map(Command::usage)
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "{commands}\nType / to discover commands; Tab completes without executing. Ctrl-V pastes files, image or text. !COMMAND shares shell output; !!COMMAND excludes it from model context"
        )
    }
}

struct Suggestion {
    name: String,
    description: String,
}

struct Menu {
    prefix: String,
    range: Range<usize>,
    items: Vec<Suggestion>,
    selected: usize,
    drawn: bool,
}

#[derive(Default)]
pub(crate) struct Completion {
    menu: Option<Menu>,
    dismissed: Option<String>,
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
        force: bool,
    ) {
        let start = draft.len() - draft.trim_start().len();
        let end = draft[start..]
            .find(char::is_whitespace)
            .map_or(draft.len(), |at| start + at);
        if cursor <= start || cursor > end || !draft[start..].starts_with('/') {
            self.clear();
            return;
        }
        let prefix = &draft[start + 1..cursor];
        if self.dismissed.as_deref() == Some(prefix) && !force {
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
        items.sort_by(|a, b| a.name.cmp(&b.name));
        let previous = self
            .menu
            .as_ref()
            .filter(|menu| menu.prefix == prefix && menu.range == (start..end));
        if items.is_empty()
            || (!force
                && previous.is_none()
                && cursor == end
                && items.iter().any(|item| item.name == prefix))
        {
            self.menu = None;
            return;
        }
        let drawn = previous.is_some_and(|menu| menu.drawn);
        let selected_name = previous
            .and_then(|menu| menu.items.get(menu.selected))
            .map(|item| &item.name);
        let selected = selected_name
            .and_then(|name| items.iter().position(|item| &item.name == name))
            .unwrap_or(0);
        self.menu = Some(Menu {
            prefix: prefix.into(),
            range: start..end,
            items,
            selected,
            drawn,
        });
    }

    pub(crate) fn active(&self) -> bool {
        self.menu.is_some()
    }

    pub(crate) fn visible(&self) -> bool {
        self.menu.as_ref().is_some_and(|menu| menu.drawn)
    }

    pub(crate) fn unique(&self) -> bool {
        self.menu.as_ref().is_some_and(|menu| menu.items.len() == 1)
    }

    pub(crate) fn move_selection(&mut self, down: bool) {
        if let Some(menu) = &mut self.menu {
            menu.selected = if down {
                (menu.selected + 1).min(menu.items.len() - 1)
            } else {
                menu.selected.saturating_sub(1)
            };
        }
    }

    pub(crate) fn dismiss(&mut self) {
        if let Some(menu) = self.menu.take() {
            self.dismissed = Some(menu.prefix);
        }
    }

    pub(crate) fn replacement(&self, draft: &str) -> Option<(Range<usize>, String, usize)> {
        let menu = self.menu.as_ref()?;
        let name = &menu.items[menu.selected].name;
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
        menu.drawn = budget >= 2;
        if !menu.drawn {
            return Vec::new();
        }
        let visible = budget.saturating_sub(1).min(4);
        let start = menu.selected.saturating_sub(visible.saturating_sub(1));
        let mut rows = vec![fit_line(
            "Commands · ↑/↓ select · Tab complete · Esc close",
            width,
        )];
        rows.extend(menu.items.iter().enumerate().skip(start).take(visible).map(
            |(index, item)| {
                fit_line(
                    &format!(
                        "{} /{} — {}",
                        if index == menu.selected { '›' } else { ' ' },
                        item.name,
                        item.description
                    ),
                    width,
                )
            },
        ));
        rows
    }
}
