# ACP Integration

Updated: 2026-03-25

ACP is a **secondary feature** for subscription access. See `ai/DESIGN.md` for the full architecture and `subscription-providers.md` for the provider table and ToS rationale.

---

## Protocol

Agent Client Protocol — JSON-RPC 2.0 over stdio.

- Spec: https://agentclientprotocol.com
- Go SDK: `github.com/coder/acp-go-sdk` (Apache 2.0, by Coder)

Ion is the ACP **client**. External CLI agents (claude, gemini, gh) are ACP **agents**.

---

## Implementation status

`internal/backend/acp/session.go` — real ACP JSON-RPC 2.0 via `github.com/coder/acp-go-sdk`. Protocol layer complete.

**Working:**

- `SessionUpdate` → session.Event mapping (text, thought, tool call/result, plan)
- `session/new` carries Ion initial context in `_meta.ion`: cwd, branch,
  model, Ion session id, resume hint, and project instruction text when present
- `RequestPermission` → synchronous approval bridge (blocks on `chan bool`)
- `ReadTextFile` / `WriteTextFile` filesystem bridge
- Terminal bridge (Create/Output/Wait/Kill/Release)
- Tests using in-process Go mock agent

**Not yet wired:**

- Provider-based backend selection is now implemented in `cmd/ion/main.go`
- Remaining ACP gaps are session continuity and token usage

---

## Event mapping

| ACP Update                     | session.Event                     |
| ------------------------------ | --------------------------------- |
| `AgentMessageChunk` (text)     | `AgentDelta{Delta}`               |
| `AgentThoughtChunk` (text)     | `ThinkingDelta{Delta}`            |
| `ToolCall` (new)               | `ToolCallStarted{ToolName, Args}` |
| `ToolCallUpdate` (in_progress) | `ToolOutputDelta{Delta}`          |
| `ToolCallUpdate` (completed)   | `ToolResult{Result}`              |
| `ToolCallUpdate` (failed)      | `ToolResult{Error}`               |
| `Plan` entries                 | `StatusChanged{Status}`           |
| `RequestPermission`            | `ApprovalRequest{...}`            |
| Prompt RPC returns             | `TurnFinished{}`                  |
| Error                          | `Error{Err}`                      |

---

## Known gaps

| Task    | Description                                                      |
| ------- | ---------------------------------------------------------------- |
| tk-6zy3 | No token usage (no standard ACP mechanism)                       |

Session continuity/resume is still an open ACP concern, but it is not currently tracked as a standalone task.

These are all secondary. Do not let them block native ion work.

---

## ACP client interface (for reference)

```go
type Client interface {
    SessionUpdate(ctx, SessionNotification) error
    RequestPermission(ctx, RequestPermissionRequest) (RequestPermissionResponse, error)
    ReadTextFile(ctx, ReadTextFileRequest) (ReadTextFileResponse, error)
    WriteTextFile(ctx, WriteTextFileRequest) (WriteTextFileResponse, error)
    CreateTerminal(ctx, CreateTerminalRequest) (CreateTerminalResponse, error)
    TerminalOutput(ctx, TerminalOutputRequest) (TerminalOutputResponse, error)
    WaitForTerminalExit(ctx, WaitForTerminalExitRequest) (WaitForTerminalExitResponse, error)
    KillTerminalCommand(ctx, KillTerminalCommandRequest) (KillTerminalCommandResponse, error)
    ReleaseTerminal(ctx, ReleaseTerminalRequest) (ReleaseTerminalResponse, error)
}
```

---

## Open questions (figure out as we build)

- **Token usage**: check `_tokenUsage` extension notification per agent.
- **Session resume**: does `AgentCapabilities.LoadSession` support resume by ID, and is it worth prioritizing before headless mode?
- **Model passthrough**: can ion pass a preferred model via `SetSessionModel`?
- **Feature bridging**: which native ion tools (sub-agents, memory) can be exposed as ACP-callable tools? Figure out after those features exist in native mode.
