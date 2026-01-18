# Session Context

## Recently Completed

All cleanup and dependency upgrade tasks completed:

- tk-vpj4: Updated GitHub repo description
- tk-btof: Cleaned up README
- tk-x3vt: Evaluated nightly (still required - omendb in core)
- tk-ykpu: Upgraded grep tool to use ignore crate
- tk-cfmz: Upgraded glob tool to use globset
- tk-ha1x: Removed unused deps (walkdir, serde_yaml, glob, async-recursion)
- tk-9tkf: Replaced tiktoken-rs with bpe-openai

## Open Tasks

| ID      | Category | Task                                           |
| ------- | -------- | ---------------------------------------------- |
| tk-3jba | BUG      | Ctrl+C not interruptible during tool execution |
| tk-smqs | IDEA     | Diff highlighting for edits                    |
| tk-otmx | UX       | Ctrl+G opens input in external editor          |
| tk-whde | UX       | Git diff stats in status line                  |
| tk-arh6 | UX       | Tool execution not visually obvious            |
| tk-o4uo | UX       | Modal escape handling                          |

## Project State

- Phase: 5 - Polish & UX
- Status: Runnable
- Tests: 51 passing
- Branch: main

## Commands

```bash
cargo build              # Debug build
cargo test               # Run tests
tk ls                    # See all tasks
tk start <id>            # Start a task
tk done <id>             # Complete a task
```
