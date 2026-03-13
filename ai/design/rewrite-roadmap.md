# Rewrite Roadmap

## Phase 1: Host Shell

- transcript viewport
- multiline composer
- footer/status/progress
- stable scrolling and resize behavior
- tool transcript rendering

## Phase 2: Session Boundary

- canonical `AgentSession` interface
- typed event model
- scripted backend aligned with the interface

## Phase 3: Native Ion Backend

- direct turn execution
- streaming assistant output
- tool and progress events
- clean cancel/error/finish states

## Phase 4: ACP Backend

- external ACP agent support
- protocol adapters kept behind the session interface
- support for subscription-safe hosted agents

## Phase 5: Persistence and Higher-Order Runtime

- transcript/session persistence
- memory and context system
- subagents, swarms, and RLM patterns
