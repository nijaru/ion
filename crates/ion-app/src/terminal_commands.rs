//! Terminal command identity and its authoritative help/discovery inventory.

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

pub(crate) struct Command {
    kind: Builtin,
    group: Group,
    pub(crate) name: &'static str,
    pub(crate) hint: &'static str,
    pub(crate) description: &'static str,
}

// Keep dispatch identities, help and discovery in one searchable inventory.
macro_rules! commands {
    ($($kind:ident => ($group:ident, $name:literal, $hint:literal, $description:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum Builtin { $($kind),+ }
        pub(crate) const COMMANDS: &[Command] = &[
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
                    "  @path suggests project files; typing stays in the editor.\n",
                    "  Tab/Enter completes a visible file; Esc closes suggestions or views.\n",
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
