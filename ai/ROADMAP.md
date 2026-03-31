# ion Roadmap

## Completed foundation

### Runtime

- canto is the native runtime
- SQLite-backed session persistence is in place
- streaming turns, tool calls, cancellation, and resume work
- layered project instructions are implemented

### TUI

- inline scrollback architecture is in place
- committed transcript rows go to terminal scrollback
- Plane B is limited to live UI
- provider/model switching works in-session
- model and provider pickers are usable and catalog-driven

### Provider foundation

- provider catalog is the source of truth
- broad native provider coverage is wired
- local and custom OpenAI-compatible endpoints are separate concepts
- direct native model fetchers exist for the main providers

## Active roadmap

### 1. Provider validation

Goal:
- turn broad structural provider support into validated provider support

Tracked by:
- `tk-ekao`

Includes:
- auth/env reality
- endpoint defaults
- `/models` behavior
- manual-entry fallbacks where live listing is weak

### 2. Instruction and skills surface

Goal:
- keep instruction layering clear
- define whether and how ion should expose first-class skills

Tracked by:
- `tk-lmhg`

### 3. Session navigation and agent breadth

Tracked by:
- `tk-0dwv` — session tree navigation
- `tk-wz8y` — sub-agent spawning
- `tk-st4q` — ion as an ACP agent

### 4. Footer and settings restraint

Goal:
- keep footer/settings useful without turning ion into a control panel

Tracked by:
- `tk-i207`

## Explicitly lower priority

- token usage color bands
- git diff stats in footer
- AskUser UI
- tab completion
- canto upstreaming tasks

These are real tasks, but they are not on the critical path.
