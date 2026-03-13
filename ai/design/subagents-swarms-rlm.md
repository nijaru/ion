# Subagents, Swarms, and RLM

## Summary

Advanced agent-runtime features remain part of the product direction, but they should be built into the native ion runtime behind the session interface.

## Principles

- the host should render these capabilities, not implement them
- subagents and swarms are runtime orchestration concerns
- RLM-style recursive decomposition belongs in planning/execution logic, not UI state

## Expected Runtime Outputs

The host should eventually be able to render:

- plan decomposition
- spawned worker/subagent progress
- review loops
- memory/context fetches
- tool execution trees
- final synthesized output

## Near-Term Work

1. define the runtime event shapes needed for advanced orchestration
2. keep the host’s session/event model expressive enough for these future patterns
3. avoid hard-coding a simple chat-only event model now
