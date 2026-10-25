//! Terminal command identity, discoverable resources and non-executing completion.
use std::ops::Range;

use ion_host::{CommandCatalog, Resources};

use crate::display_text::fit_line;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Sessions,
    Models,
    Work,
    Editor,
    Resources,
    General,
}

const GROUPS: &[(Group, &str)] = &[
    (Group::Sessions, "Sessions"),
    (Group::Models, "Models & credentials"),
    (Group::Work, "Tools & background work"),
    (Group::Editor, "Draft & display"),
    (Group::Resources, "Resources"),
    (Group::General, "Help & exit"),
];

struct Command {
    kind: Builtin,
    group: Group,
    name: &'static str,
    hint: &'static str,
    description: &'static str,
}

// Keep dispatch identities, help and discovery in one searchable inventory.
macro_rules! commands {
    ($($kind:ident => ($group:ident, $name:literal, $hint:literal, $description:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum Builtin { $($kind),+ }
        const COMMANDS: &[Command] = &[
            $(Command { kind: Builtin::$kind, group: Group::$group, name: $name, hint: $hint, description: $description }),+
        ];
    };
}
commands! {
    Help => (General, "help", "[COMMAND|keys]", "Show commands and keyboard controls"),
    New => (Sessions, "new", "", "Start a new Session"),
    Clone => (Sessions, "clone", "", "Copy this conversation into a new Session"),
    Tree => (Sessions, "tree", "[ENTRY]", "Jump to a message; preserve alternatives and current files"),
    Fork => (Sessions, "fork", "[TURN]", "Fork before a Turn and restore its input"),
    ForkAfter => (Sessions, "fork-after", "[TURN]", "Fork after a Turn"),
    Resume => (Sessions, "resume", "[ID]", "Choose a saved Session"),
    Session => (Sessions, "session", "", "Show Session and context information"),
    Name => (Sessions, "name", "[NAME]", "Show or change the Session name"),
    Model => (Models, "model", "[PROVIDER/MODEL]", "Choose a model"),
    Reasoning => (Models, "reasoning", "[default|off|low|medium|high|budget:TOKENS]", "Select generation effort (route support varies)"),
    Compact => (Work, "compact", "", "Summarize settled context"),
    Tasks => (Work, "tasks", "[status|result|cancel|acknowledge UUID]", "Choose task progress, result, stop or remove"),
    Attach => (Work, "attach", "[ID]", "Attach to a running PTY program; Ctrl-] detaches"),
    Tools => (Work, "tools", "", "List recorded tool operations"),
    Tool => (Work, "tool", "[N]", "Inspect recorded tool evidence"),
    Settings => (Editor, "settings", "[compact|expanded|thinking show|thinking hide]", "Change terminal presentation"),
    Tui => (Editor, "tui", "[inline|fullscreen]", "Choose the transcript surface"),
    Image => (Editor, "image", "PATH", "Attach an image to the next input"),
    Copy => (Editor, "copy", "", "Copy the last completed answer"),
    Editor => (Editor, "editor", "", "Edit the draft in an external editor"),
    Export => (Editor, "export", "PATH", "Save a readable transcript"),
    Skills => (Resources, "skills", "", "List available skills"),
    Prompts => (Resources, "prompts", "", "List available prompt templates"),
    Commands => (Resources, "commands", "", "List enabled executable user commands"),
    Reload => (Resources, "reload", "", "Reload project instructions, resources and command metadata"),
    Login => (Models, "login", "[PROVIDER]", "Choose an API-key provider or save its key using masked input"),
    Logout => (Models, "logout", "PROVIDER", "Remove a saved credential"),
    Quit => (General, "quit", "", "Exit the terminal client"),
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
        let groups = GROUPS
            .iter()
            .map(|(group, title)| {
                let names = COMMANDS
                    .iter()
                    .filter(|command| command.group == *group)
                    .map(|command| format!("/{}", command.name))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{title}: {names}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Commands\n\n{groups}\n\nType / to discover commands; Tab completes without executing.\n/help COMMAND shows usage; /help keys shows keyboard controls.\nEnter sends · Shift-Enter adds a line · Ctrl-C stops work"
        )
    }

    pub(crate) fn help_for(name: &str) -> Option<String> {
        if name == "keys" {
            return Some(
                concat!(
                    "Keyboard controls\n",
                    "\n",
                    "Navigation\n",
                    "  Arrows move; Up/Down at the first/last line browse prompt history.\n",
                    "  Home/End or Ctrl-A/E move to line boundaries.\n",
                    "  Ctrl/Alt-Left/Right or Alt-B/F move by word.\n",
                    "  Page-Up/Down scroll the fullscreen transcript.\n",
                    "\n",
                    "Editing\n",
                    "  Enter sends; Shift/Ctrl-Enter or Ctrl-J adds a line.\n",
                    "  Ctrl-V pastes files, image or text; Ctrl-G opens the external editor.\n",
                    "  Tab after @path opens file search; Esc dismisses completion or a view.\n",
                    "  Ctrl-U/K kills to line start/end; Ctrl-W or Alt-Backspace kills a word.\n",
                    "  Alt-D/Delete kills the next word; Ctrl-Y yanks; Alt-Y cycles text kills.\n",
                    "  Ctrl-minus or Ctrl-_ undoes; adding Alt redoes.\n",
                    "  Ctrl-D deletes forward, or exits with an empty draft.\n",
                    "  Ctrl-C while idle clears the draft, or exits when it is empty.\n",
                    "  Ctrl-Z suspends on Unix; fg resumes the same chat.\n",
                    "\n",
                    "Views & evidence\n",
                    "  Ctrl-L chooses a model without replacing the draft.\n",
                    "  Ctrl-O opens conversation details; Esc or Ctrl-O closes them.\n",
                    "  Ctrl-X copies the last answer, or selected tool evidence in its detail view.\n",
                    "\n",
                    "During work\n",
                    "  Enter steers a coding Turn; Alt-Enter queues a follow-up.\n",
                    "  Enter during shell or compaction queues a follow-up.\n",
                    "  Alt-Up recovers queued or returned drafts; Ctrl-C cancels work.\n",
                    "\n",
                    "Shell\n",
                    "  !COMMAND shares shell output; !!COMMAND excludes it from model context.",
                )
                .into(),
            );
        }
        let name = name.strip_prefix('/').unwrap_or(name);
        let name = if name == "exit" { "quit" } else { name };
        COMMANDS
            .iter()
            .find(|command| command.name == name)
            .map(|command| format!("{}\nUsage: {}", command.description, command.usage()))
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
        commands: Option<&CommandCatalog>,
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
