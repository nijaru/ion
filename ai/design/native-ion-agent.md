# Native Ion Agent

## Summary

`NativeIonSession` is the in-house backend that runs ion’s own agent logic behind the same host-facing session boundary.

## Responsibilities

It should own:

- direct provider/API usage
- tool execution
- transcript production
- progress/plan events
- approvals if needed
- persistence hooks
- memory/context integration
- subagent, swarm, and RLM orchestration later

## Near-Term Scope

The first native backend should be intentionally small:

- submit a turn
- stream assistant text
- emit tool/progress events
- finish or fail the turn cleanly
- preserve enough state to support later persistence
