# Changelog

## [0.1.0] - 2026-09-06

First dogfood-quality slice of the Ion terminal agent: the durable
single-writer session runtime, the model-facing agent loop, and the
TUI.

- Durable session store (SQLite): entries, lanes, agents, effects,
  model steps, usage, and turn checkpoints with crash recovery.
- Agent loop with OpenRouter and OpenAI Codex providers, per-lane
  thinking levels, and git-checkpointed forks.
- TUI: streaming transcript, model picker, session manager, image
  paste, hotkeys reference, fullscreen mode, and footer stats.
- Tools: bash, read, write, edit, find, grep, and ls with protected
  path policy and workspace sandbox modes.
- Pi-parity commands: /help /hotkeys /model /thinking /name /new
  /resume /clone /fork /session /compact /copy /export /import
  /debug /changelog /share /quit.
