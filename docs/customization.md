# Skills, prompts and commands

Ion supports Markdown skills, reusable prompt templates and explicit executable
commands. You can discover and use them from terminal chat without changing the
agent engine or writing a plugin.

In chat, `/skills`, `/prompts` and `/commands` list the current catalog. Type `/`
to browse command completions; Tab completes a choice without submitting it.
`/reload` refreshes project instructions, skills, prompts, command metadata,
keybindings and themes. It reports loaded counts and the files and reasons for
warnings. Reloading and listing resources do not invoke a model or run a command.

## Add a skill

Create `.agents/skills/review/SKILL.md` in your project:

```markdown
---
name: review
description: Review a change for correctness and missing regression protection.
---
Read the relevant source and tests. Report concrete defects with file locations.
Do not edit files unless asked.
```

Run `/reload`, then `/skills`. Invoke the skill explicitly with:

```text
/skill:review Check the current diff
```

Descriptions of model-invocable skills are included in the model's instructions;
the model can read a skill when its description applies. An explicit
`/skill:review` invocation includes the skill body and your request in a new
prompt. Set `disable-model-invocation: true` in the frontmatter to omit a skill
from model discovery while keeping explicit invocation available. This does not
restrict the model's filesystem permissions.

Default skill locations are:

- The active Ion home's `skills/` directory (`~/.ion/skills/` by default).
- `~/.agents/skills/`.
- Project `.agents/skills/` directories, bounded by the nearest Git root.

To select other files or directories at startup:

```sh
ion --skill ./my-review.md --skill ./team-skills
ion --no-skills --skill ./team-skills
```

`--no-skills` disables default discovery, not explicit paths. `/reload` keeps the
startup selection and rereads its sources. `/skills` shows the selected files.

## Add a prompt template

Create `.ion/prompts/review-change.md` in your project:

```markdown
---
description: Review a named target
argument-hint: TARGET
---
Review $1 for correctness. Limit the review to ${2:-the current diff}.
```

After `/reload`, use `/prompts` or type `/review-change` to discover it:

```text
/review-change src/parser.rs "error handling"
```

Templates support positional arguments such as `$1`, all arguments with `$@`,
and defaults such as `${2:-the current diff}`. These are text substitutions, not
shell execution or environment expansion. Quoted arguments keep spaces together.
Expanded arguments are not recursively interpreted as another resource command.
Choose a name that does not conflict with a built-in slash command; terminal
chat reports conflicts rather than advertising the template as usable.

Default templates come from the active Ion home's `prompts/` directory and
project `.ion/prompts/` directories. Each template filename supplies its command
name. Template directories are shallow: put templates directly in the directory.
Explicit startup selection works independently of skill selection:

```sh
ion --prompt-template ./review-change.md --prompt-template ./team-prompts
ion --no-prompt-templates --prompt-template ./team-prompts
```

## Add an executable command

For a personal command, put this in the active Ion home's `commands.json`
(`~/.ion/commands.json` by default):

```json
{
  "commands": {
    "test": {
      "description": "Run the Rust test suite",
      "program": "cargo",
      "args": ["test"],
      "share_with_model": false
    }
  }
}
```

After `/reload`, list it with `/commands` and run it with:

```text
/command:test --locked
```

Typed arguments are appended as literal arguments. Discovery and reload never
launch the configured program; invocation uses Ion's existing direct-shell path.
With `share_with_model: false`, the command and result stay outside model context.
Set it to `true` only when you want subsequent model requests to see them.

A project can supply `.ion/commands.json`, but it is inactive until you review
and explicitly trust its exact contents from that project directory:

```sh
ion commands list
ion commands trust
```

Then `/reload` in chat. Changing the project manifest invalidates its grant;
review and trust the new contents before reloading. `ion commands untrust`
revokes the grant. Executable commands run with your host permissions and are
unavailable in a read-only coding runtime; they are not sandboxed plugins.

`ION_HOME` changes the active Ion home. See `ion resources --help`,
`ion commands --help` and `ion --help` for the complete CLI options.
