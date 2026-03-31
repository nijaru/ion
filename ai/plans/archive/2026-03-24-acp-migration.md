# ACP Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace `internal/backend/acp/` custom hand-rolled protocol with real Agent Client Protocol using `github.com/coder/acp-go-sdk`. Ion acts as the ACP **client**; external agents (Claude Code, Gemini CLI, Codex CLI) act as ACP **agents** communicating over JSON-RPC 2.0 on stdio.

**Architecture:** ion spawns an agent process (configured by `ION_ACP_COMMAND`), connects via `NewClientSideConnection`, implements the `Client` interface to translate ACP events → `session.Event`, and handles bidirectional requests (permission, file read/write, terminal). The existing `backend.go` and its `Backend` interface implementation stay largely unchanged; only `session.go` is rewritten.

**Tech Stack:** Go 1.26, `github.com/coder/acp-go-sdk`, existing `internal/session` event types, existing `backend.PolicyEngine`

**Design doc:** `ai/design/acp-integration.md`

---

## File Map

| File                                   | Action          | Responsibility                                                                                      |
| -------------------------------------- | --------------- | --------------------------------------------------------------------------------------------------- |
| `internal/backend/acp/session.go`      | Rewrite         | ACP client connection, Client interface, event mapping, approval bridge, fs bridge, terminal bridge |
| `internal/backend/acp/backend.go`      | Minimal changes | Preserve `Backend` struct; update `Open`/`Resume` calls if needed                                   |
| `internal/backend/acp/backend_test.go` | Rewrite         | Go-based mock agent using SDK's agent-side connection                                               |

No changes to `cmd/ion/main.go`, `internal/app/`, or `internal/session/`.

---

## Task 1: Add SDK dependency and compile check

**Files:**

- Modify: `go.mod`, `go.sum`

- [ ] **Step 1: Add the SDK**

```bash
cd /Users/nick/github/nijaru/ion && go get github.com/coder/acp-go-sdk
```

- [ ] **Step 2: Verify existing code still builds**

```bash
go build ./...
```

Expected: passes. The SDK is additive — nothing is wired yet.

- [ ] **Step 3: Commit**

```bash
git add go.mod go.sum
git commit -m "build(deps): add github.com/coder/acp-go-sdk"
```

---

## Task 2: Rewrite session.go — connection and lifecycle

**Files:**

- Rewrite: `internal/backend/acp/session.go`

The current `Session` speaks a custom protocol. Replace entirely. The new session:

- Spawns agent via `ION_ACP_COMMAND`
- Connects with `acp.NewClientSideConnection`
- Implements `acp.Client` interface
- Translates events → `session.Event` channel
- Handles approval bridging via pending channel map

- [ ] **Step 1: Write the new session.go skeleton**

```go
package acp

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"sync"

	acp "github.com/coder/acp-go-sdk"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// Session is ion's ACP client session. It spawns an external agent process,
// connects via JSON-RPC 2.0 over stdio, and translates ACP events to session.Event.
type Session struct {
	events chan session.Event
	store  storage.Store
	storage storage.Session
	policy *backend.PolicyEngine

	conn      *acp.ClientSideConnection
	sessionID string
	cmd       *exec.Cmd
	cancel    context.CancelFunc
	mu        sync.Mutex

	// Pending approval requests: requestID → response channel
	pendingApprovals map[string]chan bool
}

func newSession() *Session {
	return &Session{
		events:           make(chan session.Event, 100),
		policy:           backend.NewPolicyEngine(),
		pendingApprovals: make(map[string]chan bool),
	}
}

func (s *Session) Open(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	command := os.Getenv("ION_ACP_COMMAND")
	if command == "" {
		return fmt.Errorf("ION_ACP_COMMAND environment variable not set")
	}

	ctx, cancel := context.WithCancel(ctx)
	s.cancel = cancel

	cmd := exec.CommandContext(ctx, "bash", "-c", command)
	s.cmd = cmd

	stdin, err := cmd.StdinPipe()
	if err != nil {
		cancel()
		return fmt.Errorf("stdin pipe: %w", err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		cancel()
		return fmt.Errorf("stdout pipe: %w", err)
	}
	// Discard stderr — agent stderr is the agent's concern, not ion's event stream
	cmd.Stderr = nil

	if err := cmd.Start(); err != nil {
		cancel()
		return fmt.Errorf("start agent: %w", err)
	}

	s.conn = acp.NewClientSideConnection(s, stdin, stdout)

	// Initialize — advertise ion's capabilities
	_, err = s.conn.Initialize(ctx, acp.InitializeRequest{
		ProtocolVersion: 1,
		ClientCapabilities: acp.ClientCapabilities{
			Fs: acp.FileSystemCapability{
				ReadTextFile:  true,
				WriteTextFile: true,
			},
			Terminal: true,
		},
	})
	if err != nil {
		cancel()
		return fmt.Errorf("acp initialize: %w", err)
	}

	// Create session
	resp, err := s.conn.NewSession(ctx, acp.NewSessionRequest{
		Cwd: cwdFromStorage(s.storage),
	})
	if err != nil {
		cancel()
		return fmt.Errorf("acp new session: %w", err)
	}
	s.sessionID = resp.SessionId

	// Emit TurnFinished equivalent so the TUI starts in Ready state
	s.events <- session.TurnFinished{}

	// Reap process in background
	go func() { _ = cmd.Wait() }()

	return nil
}

func (s *Session) Resume(ctx context.Context, sessionID string) error {
	// TODO: pass sessionID to agent via env or Initialize metadata once the
	// ACP spec defines session continuity. For now, open a fresh session.
	return s.Open(ctx)
}

func (s *Session) Close() error {
	if s.cancel != nil {
		s.cancel()
	}
	close(s.events)
	return nil
}

func (s *Session) Events() <-chan session.Event { return s.events }

func (s *Session) ID() string {
	if s.storage != nil {
		return s.storage.ID()
	}
	return s.sessionID
}

func (s *Session) Meta() map[string]string {
	if s.storage != nil {
		m := s.storage.Meta()
		return map[string]string{
			"model":  m.Model,
			"branch": m.Branch,
			"cwd":    m.CWD,
		}
	}
	return nil
}

func cwdFromStorage(stor storage.Session) string {
	if stor != nil {
		if m := stor.Meta(); m.CWD != "" {
			return m.CWD
		}
	}
	cwd, _ := os.Getwd()
	return cwd
}
```

- [ ] **Step 2: Build to check for compile errors**

```bash
go build ./internal/backend/acp/...
```

Expected: compile errors about missing `Client` interface methods. That's expected.

- [ ] **Step 3: Commit skeleton**

```bash
git add internal/backend/acp/session.go
git commit -m "feat(acp): skeleton — NewClientSideConnection replaces custom protocol"
```

---

## Task 3: Implement SessionUpdate — core event mapping

**Files:**

- Modify: `internal/backend/acp/session.go`

`SessionUpdate` is how the agent streams all updates to ion. It maps directly to `session.Event`.

- [ ] **Step 1: Write failing test** (add to `backend_test.go`)

```go
func TestACPSessionUpdateMapping(t *testing.T) {
	// Spin up a minimal Go-based mock agent using the SDK
	client, agentEvents := newTestPair(t)

	// Agent sends a text chunk
	agentEvents <- acp.SessionNotification{
		SessionId: "s1",
		Update:    acp.UpdateAgentMessageText("hello"),
	}

	ev := drainOne(t, client.events, 500*time.Millisecond)
	delta, ok := ev.(session.AssistantDelta)
	if !ok {
		t.Fatalf("expected AssistantDelta, got %T", ev)
	}
	if delta.Delta != "hello" {
		t.Errorf("expected 'hello', got %q", delta.Delta)
	}
}
```

(Helpers `newTestPair` and `drainOne` are added in Task 7.)

- [ ] **Step 2: Implement SessionUpdate**

```go
// SessionUpdate implements acp.Client. Called by the SDK when the agent sends any update.
func (s *Session) SessionUpdate(ctx context.Context, n acp.SessionNotification) error {
	update := n.Update

	switch {
	case update.AgentMessageChunk != nil:
		chunk := update.AgentMessageChunk
		if chunk.Content.Text != nil {
			s.events <- session.AssistantDelta{Delta: chunk.Content.Text.Text}
		}
		if chunk.Done {
			// TurnFinished is sent separately; this marks end of a message chunk stream
		}

	case update.AgentMessage != nil:
		// Full completed message (may arrive instead of chunks for non-streaming agents)
		msg := update.AgentMessage
		if msg.Content.Text != nil {
			s.events <- session.AssistantMessage{Message: msg.Content.Text.Text}
		}

	case update.ToolCall != nil:
		tc := update.ToolCall
		switch {
		case tc.Start != nil:
			s.events <- session.ToolCallStarted{
				ToolName: string(tc.ToolCallId),
				Args:     tc.Start.Title,
			}
		case tc.Update != nil && tc.Update.RawOutput != nil:
			// Tool produced output
			if out, ok := (*tc.Update.RawOutput)["output"].(string); ok {
				s.events <- session.ToolOutputDelta{Delta: out}
			}
		case tc.Update != nil && tc.Update.Status != nil:
			if *tc.Update.Status == acp.ToolCallStatusCompleted {
				output := ""
				if tc.Update.RawOutput != nil {
					if out, ok := (*tc.Update.RawOutput)["output"].(string); ok {
						output = out
					}
				}
				s.events <- session.ToolResult{Result: output}
			} else if *tc.Update.Status == acp.ToolCallStatusFailed {
				s.events <- session.ToolResult{
					Result: "tool failed",
					Error:  fmt.Errorf("tool call failed"),
				}
			}
		}

	case update.StatusUpdate != nil:
		s.events <- session.StatusChanged{Status: update.StatusUpdate.Status}

	case update.Plan != nil:
		// PlanUpdated — emit as StatusChanged for now (plan rendering is future work)
		if len(update.Plan.Entries) > 0 {
			s.events <- session.StatusChanged{Status: update.Plan.Entries[0].Title}
		}

	case update.Done != nil:
		s.events <- session.TurnFinished{}
	}

	return nil
}
```

Note: The exact field names depend on the SDK version. Check `pkg.go.dev/github.com/coder/acp-go-sdk`
for the actual `SessionUpdate` union type structure before finalizing.

- [ ] **Step 3: Build**

```bash
go build ./internal/backend/acp/...
```

- [ ] **Step 4: Commit**

```bash
git add internal/backend/acp/session.go
git commit -m "feat(acp): implement SessionUpdate — ACP events → session.Event"
```

---

## Task 4: Implement SubmitTurn, CancelTurn, Approve

**Files:**

- Modify: `internal/backend/acp/session.go`

- [ ] **Step 1: Implement turn methods**

```go
func (s *Session) SubmitTurn(ctx context.Context, input string) error {
	s.mu.Lock()
	conn := s.conn
	sessionID := s.sessionID
	s.mu.Unlock()

	if conn == nil {
		return fmt.Errorf("not connected")
	}

	s.events <- session.TurnStarted{}

	_, err := conn.Prompt(ctx, acp.PromptRequest{
		SessionId: acp.SessionId(sessionID),
		Prompt:    []acp.ContentBlock{acp.TextBlock(input)},
	})
	return err
}

func (s *Session) CancelTurn(ctx context.Context) error {
	s.mu.Lock()
	conn := s.conn
	sessionID := s.sessionID
	s.mu.Unlock()

	if conn == nil {
		return nil
	}

	return conn.Cancel(ctx, acp.CancelNotification{
		SessionId: acp.SessionId(sessionID),
	})
}

// Approve resolves a pending RequestPermission call that is blocking the agent.
func (s *Session) Approve(ctx context.Context, requestID string, approved bool) error {
	s.mu.Lock()
	ch, ok := s.pendingApprovals[requestID]
	if ok {
		delete(s.pendingApprovals, requestID)
	}
	s.mu.Unlock()

	if ok {
		ch <- approved
	}
	return nil
}

func (s *Session) RegisterMCPServer(ctx context.Context, command string, args ...string) error {
	// TODO: forward via ACP extension method once standardized
	// For now, not supported for ACP agents
	return fmt.Errorf("MCP server registration not yet supported for ACP agents")
}
```

- [ ] **Step 2: Commit**

```bash
git add internal/backend/acp/session.go
git commit -m "feat(acp): implement SubmitTurn, CancelTurn, Approve"
```

---

## Task 5: Implement RequestPermission (approval bridge)

**Files:**

- Modify: `internal/backend/acp/session.go`

`RequestPermission` is called **synchronously by the agent** — it blocks waiting for ion's response.
Ion must emit `ApprovalRequest`, wait for `Approve()` to be called, then return the outcome.

- [ ] **Step 1: Write failing test** (add to `backend_test.go`)

```go
func TestACPApprovalBridge(t *testing.T) {
	client, _ := newTestPair(t)

	// Simulate agent calling RequestPermission concurrently
	approved := make(chan bool, 1)
	go func() {
		resp, err := client.conn.RequestPermission(context.Background(), acp.RequestPermissionRequest{
			SessionId: "s1",
			ToolCall: acp.RequestPermissionToolCall{
				ToolCallId: "call_1",
				Kind:       acp.Ptr(acp.ToolKindEdit),
			},
			Options: []acp.PermissionOption{
				{Kind: acp.PermissionOptionKindAllowOnce, OptionId: "allow", Name: "Allow"},
				{Kind: acp.PermissionOptionKindRejectOnce, OptionId: "reject", Name: "Reject"},
			},
		})
		if err != nil {
			return
		}
		approved <- resp.Outcome.Selected != nil && resp.Outcome.Selected.OptionId == "allow"
	}()

	// Wait for ApprovalRequest event to arrive
	ev := drainOne(t, client.events, 500*time.Millisecond)
	req, ok := ev.(session.ApprovalRequest)
	if !ok {
		t.Fatalf("expected ApprovalRequest, got %T", ev)
	}

	// Approve it
	client.Approve(context.Background(), req.RequestID, true)

	select {
	case result := <-approved:
		if !result {
			t.Error("expected approved=true")
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("timed out waiting for approval response")
	}
}
```

- [ ] **Step 2: Implement RequestPermission**

```go
// RequestPermission implements acp.Client. Called by the agent when it needs permission
// for a tool action. This blocks until ion's Approve() is called.
func (s *Session) RequestPermission(ctx context.Context, p acp.RequestPermissionRequest) (acp.RequestPermissionResponse, error) {
	requestID := string(p.ToolCall.ToolCallId)

	// Check policy engine first — may auto-approve or auto-deny
	var toolName, args string
	if p.ToolCall.Kind != nil {
		toolName = string(*p.ToolCall.Kind)
	}
	policy, _ := s.policy.Authorize(ctx, toolName, args)
	switch policy {
	case backend.PolicyAllow:
		return allowResponse(p), nil
	case backend.PolicyDeny:
		return denyResponse(p), nil
	}

	// No policy match — ask the user
	ch := make(chan bool, 1)
	s.mu.Lock()
	s.pendingApprovals[requestID] = ch
	s.mu.Unlock()

	desc := ""
	if len(p.Options) > 0 {
		desc = string(p.Options[0].Name)
	}
	s.events <- session.ApprovalRequest{
		RequestID:   requestID,
		ToolName:    toolName,
		Description: desc,
	}

	select {
	case approved := <-ch:
		if approved {
			return allowResponse(p), nil
		}
		return denyResponse(p), nil
	case <-ctx.Done():
		return acp.RequestPermissionResponse{}, ctx.Err()
	}
}

func allowResponse(p acp.RequestPermissionRequest) acp.RequestPermissionResponse {
	for _, opt := range p.Options {
		if opt.Kind == acp.PermissionOptionKindAllowOnce || opt.Kind == acp.PermissionOptionKindAllowAlways {
			return acp.RequestPermissionResponse{
				Outcome: acp.RequestPermissionOutcome{
					Selected: &acp.RequestPermissionOutcomeSelected{OptionId: opt.OptionId},
				},
			}
		}
	}
	return acp.RequestPermissionResponse{
		Outcome: acp.RequestPermissionOutcome{Cancelled: &acp.RequestPermissionOutcomeCancelled{}},
	}
}

func denyResponse(p acp.RequestPermissionRequest) acp.RequestPermissionResponse {
	for _, opt := range p.Options {
		if opt.Kind == acp.PermissionOptionKindRejectOnce || opt.Kind == acp.PermissionOptionKindRejectAlways {
			return acp.RequestPermissionResponse{
				Outcome: acp.RequestPermissionOutcome{
					Selected: &acp.RequestPermissionOutcomeSelected{OptionId: opt.OptionId},
				},
			}
		}
	}
	return acp.RequestPermissionResponse{
		Outcome: acp.RequestPermissionOutcome{Cancelled: &acp.RequestPermissionOutcomeCancelled{}},
	}
}
```

- [ ] **Step 3: Run tests**

```bash
go test ./internal/backend/acp/... -run TestACPApprovalBridge -v
```

- [ ] **Step 4: Commit**

```bash
git add internal/backend/acp/session.go
git commit -m "feat(acp): implement RequestPermission — synchronous approval bridge"
```

---

## Task 6: Implement filesystem bridge

**Files:**

- Modify: `internal/backend/acp/session.go`

The agent reads/writes files through ion. Writes have already been gated by `RequestPermission`
before this call arrives, so `WriteTextFile` can write directly.

- [ ] **Step 1: Implement**

```go
func (s *Session) ReadTextFile(ctx context.Context, p acp.ReadTextFileRequest) (acp.ReadTextFileResponse, error) {
	data, err := os.ReadFile(p.Path)
	if err != nil {
		return acp.ReadTextFileResponse{}, fmt.Errorf("read %s: %w", p.Path, err)
	}
	return acp.ReadTextFileResponse{Content: string(data)}, nil
}

func (s *Session) WriteTextFile(ctx context.Context, p acp.WriteTextFileRequest) (acp.WriteTextFileResponse, error) {
	// Permission was already granted via RequestPermission before this call
	if err := os.WriteFile(p.Path, []byte(p.Content), 0644); err != nil {
		return acp.WriteTextFileResponse{}, fmt.Errorf("write %s: %w", p.Path, err)
	}
	return acp.WriteTextFileResponse{}, nil
}
```

- [ ] **Step 2: Build**

```bash
go build ./internal/backend/acp/...
```

- [ ] **Step 3: Commit**

```bash
git add internal/backend/acp/session.go
git commit -m "feat(acp): implement filesystem bridge (ReadTextFile, WriteTextFile)"
```

---

## Task 7: Implement terminal bridge

**Files:**

- Modify: `internal/backend/acp/session.go`

The terminal bridge lets the agent spawn and interact with shell commands. Ion manages process
lifecycle and streams output back.

This is the most complex part. Investigate the exact `TerminalOutput` contract from the SDK
(polling vs blocking) before finalizing the implementation.

- [ ] **Step 1: Check SDK terminal semantics**

```bash
go doc github.com/coder/acp-go-sdk TerminalOutputRequest
go doc github.com/coder/acp-go-sdk TerminalOutputResponse
```

Note whether `TerminalOutput` returns buffered output (polling) or blocks until new output arrives.

- [ ] **Step 2: Implement terminal struct**

```go
type terminal struct {
	cmd    *exec.Cmd
	stdout *strings.Builder
	done   chan struct{}
	mu     sync.Mutex
}

// terminals map: terminalID → *terminal
// Add to Session struct: terminals map[string]*terminal
```

- [ ] **Step 3: Implement terminal methods**

```go
func (s *Session) CreateTerminal(ctx context.Context, p acp.CreateTerminalRequest) (acp.CreateTerminalResponse, error) {
	shell := p.Shell
	if shell == "" {
		shell = "/bin/bash"
	}

	cmd := exec.CommandContext(ctx, shell)
	var buf strings.Builder
	cmd.Stdout = &buf
	cmd.Stderr = &buf
	done := make(chan struct{})

	if err := cmd.Start(); err != nil {
		return acp.CreateTerminalResponse{}, fmt.Errorf("start terminal: %w", err)
	}

	termID := fmt.Sprintf("term-%d", cmd.Process.Pid)
	t := &terminal{cmd: cmd, stdout: &buf, done: done}

	go func() {
		_ = cmd.Wait()
		close(done)
	}()

	s.mu.Lock()
	if s.terminals == nil {
		s.terminals = make(map[string]*terminal)
	}
	s.terminals[termID] = t
	s.mu.Unlock()

	return acp.CreateTerminalResponse{TerminalId: acp.TerminalId(termID)}, nil
}

func (s *Session) TerminalOutput(ctx context.Context, p acp.TerminalOutputRequest) (acp.TerminalOutputResponse, error) {
	s.mu.Lock()
	t, ok := s.terminals[string(p.TerminalId)]
	s.mu.Unlock()
	if !ok {
		return acp.TerminalOutputResponse{}, fmt.Errorf("unknown terminal %s", p.TerminalId)
	}

	t.mu.Lock()
	output := t.stdout.String()
	t.stdout.Reset()
	t.mu.Unlock()

	return acp.TerminalOutputResponse{Output: output}, nil
}

func (s *Session) WaitForTerminalExit(ctx context.Context, p acp.WaitForTerminalExitRequest) (acp.WaitForTerminalExitResponse, error) {
	s.mu.Lock()
	t, ok := s.terminals[string(p.TerminalId)]
	s.mu.Unlock()
	if !ok {
		return acp.WaitForTerminalExitResponse{}, fmt.Errorf("unknown terminal %s", p.TerminalId)
	}

	select {
	case <-t.done:
		code := 0
		if t.cmd.ProcessState != nil {
			code = t.cmd.ProcessState.ExitCode()
		}
		return acp.WaitForTerminalExitResponse{ExitStatus: code}, nil
	case <-ctx.Done():
		return acp.WaitForTerminalExitResponse{}, ctx.Err()
	}
}

func (s *Session) KillTerminalCommand(ctx context.Context, p acp.KillTerminalCommandRequest) (acp.KillTerminalCommandResponse, error) {
	s.mu.Lock()
	t, ok := s.terminals[string(p.TerminalId)]
	s.mu.Unlock()
	if !ok {
		return acp.KillTerminalCommandResponse{}, nil
	}
	if t.cmd.Process != nil {
		_ = t.cmd.Process.Signal(os.Interrupt)
	}
	return acp.KillTerminalCommandResponse{}, nil
}

func (s *Session) ReleaseTerminal(ctx context.Context, p acp.ReleaseTerminalRequest) (acp.ReleaseTerminalResponse, error) {
	s.mu.Lock()
	t, ok := s.terminals[string(p.TerminalId)]
	if ok {
		delete(s.terminals, string(p.TerminalId))
	}
	s.mu.Unlock()

	if ok && t.cmd.Process != nil {
		_ = t.cmd.Process.Kill()
	}
	return acp.ReleaseTerminalResponse{}, nil
}
```

- [ ] **Step 4: Build**

```bash
go build ./internal/backend/acp/...
```

Expected: compiles. Adjust field names if SDK types differ from above.

- [ ] **Step 5: Commit**

```bash
git add internal/backend/acp/session.go
git commit -m "feat(acp): implement terminal bridge"
```

---

## Task 8: Rewrite backend_test.go with Go mock agent

**Files:**

- Rewrite: `internal/backend/acp/backend_test.go`

The old test used a bash script mock. Replace with a Go mock using `acp.NewAgentSideConnection`
so it speaks real ACP JSON-RPC 2.0. Add test helpers `newTestPair` and `drainOne`.

- [ ] **Step 1: Write test helpers**

```go
package acp

import (
	"context"
	"io"
	"testing"
	"time"

	acp "github.com/coder/acp-go-sdk"
	"github.com/nijaru/ion/internal/session"
)

// mockAgent is a minimal ACP agent-side implementation for tests.
type mockAgent struct {
	conn *acp.AgentSideConnection
}

func (a *mockAgent) SetAgentConnection(c *acp.AgentSideConnection) { a.conn = c }

func (a *mockAgent) Authenticate(_ context.Context, _ acp.AuthenticateRequest) (acp.AuthenticateResponse, error) {
	return acp.AuthenticateResponse{}, nil
}

func (a *mockAgent) Initialize(_ context.Context, _ acp.InitializeRequest) (acp.InitializeResponse, error) {
	return acp.InitializeResponse{ProtocolVersion: 1}, nil
}

func (a *mockAgent) Cancel(_ context.Context, _ acp.CancelNotification) error { return nil }

func (a *mockAgent) NewSession(_ context.Context, _ acp.NewSessionRequest) (acp.NewSessionResponse, error) {
	return acp.NewSessionResponse{SessionId: "test-session"}, nil
}

func (a *mockAgent) Prompt(_ context.Context, _ acp.PromptRequest) (acp.PromptResponse, error) {
	return acp.PromptResponse{StopReason: acp.StopReasonEndTurn}, nil
}

func (a *mockAgent) SetSessionMode(_ context.Context, _ acp.SetSessionModeRequest) (acp.SetSessionModeResponse, error) {
	return acp.SetSessionModeResponse{}, nil
}

// newTestPair creates a connected client+agent pair over in-process pipes.
// Returns the ion Session (client side) and a channel to inject agent notifications.
func newTestPair(t *testing.T) (*Session, chan<- acp.SessionNotification) {
	t.Helper()

	clientRead, agentWrite := io.Pipe()
	agentRead, clientWrite := io.Pipe()

	agent := &mockAgent{}
	agentConn := acp.NewAgentSideConnection(agent, agentWrite, agentRead)
	agent.SetAgentConnection(agentConn)

	client := newSession()
	client.conn = acp.NewClientSideConnection(client, clientWrite, clientRead)
	client.sessionID = "test-session"

	// Initialize handshake
	ctx := context.Background()
	go func() {
		_, _ = client.conn.Initialize(ctx, acp.InitializeRequest{ProtocolVersion: 1})
		_, _ = client.conn.NewSession(ctx, acp.NewSessionRequest{Cwd: "/tmp"})
	}()

	t.Cleanup(func() {
		_ = clientRead.Close()
		_ = clientWrite.Close()
		_ = agentRead.Close()
		_ = agentWrite.Close()
	})

	notifyCh := make(chan acp.SessionNotification, 10)
	go func() {
		for n := range notifyCh {
			_ = agentConn.SessionUpdate(ctx, n)
		}
	}()

	return client, notifyCh
}

// drainOne reads one event from the channel or fails after timeout.
func drainOne(t *testing.T, ch <-chan session.Event, timeout time.Duration) session.Event {
	t.Helper()
	select {
	case ev := <-ch:
		return ev
	case <-time.After(timeout):
		t.Fatalf("timed out waiting for event")
		return nil
	}
}
```

- [ ] **Step 2: Rewrite TestACPBackend using the new helpers**

```go
func TestACPFullTurn(t *testing.T) {
	client, agentNotify := newTestPair(t)

	// Agent streams a turn
	go func() {
		agentNotify <- acp.SessionNotification{
			SessionId: "test-session",
			Update:    acp.UpdateAgentMessageText("hello"),
		}
		agentNotify <- acp.SessionNotification{
			SessionId: "test-session",
			Update:    acp.UpdateAgentMessageText(" world"),
		}
		// Signal done
		agentNotify <- acp.SessionNotification{
			SessionId: "test-session",
			// Update with done marker — check SDK for exact type
		}
	}()

	ev1 := drainOne(t, client.events, 500*time.Millisecond)
	delta1, ok := ev1.(session.AssistantDelta)
	if !ok || delta1.Delta != "hello" {
		t.Errorf("expected AssistantDelta{hello}, got %T %v", ev1, ev1)
	}

	ev2 := drainOne(t, client.events, 500*time.Millisecond)
	delta2, ok := ev2.(session.AssistantDelta)
	if !ok || delta2.Delta != " world" {
		t.Errorf("expected AssistantDelta{ world}, got %T %v", ev2, ev2)
	}
}
```

- [ ] **Step 3: Run tests**

```bash
go test ./internal/backend/acp/... -v
```

Expected: all pass. Fix SDK type mismatches as needed.

- [ ] **Step 4: Commit**

```bash
git add internal/backend/acp/backend_test.go
git commit -m "test(acp): rewrite tests using Go mock agent via acp-go-sdk"
```

---

## Task 9: Integration — full build and smoke test

**Files:**

- Check: `cmd/ion/main.go` (may need ACP backend wiring)
- Check: `internal/backend/acp/backend.go` (may need minor updates)

- [ ] **Step 1: Full build**

```bash
go build ./...
```

- [ ] **Step 2: Full test suite**

```bash
go test ./...
```

- [ ] **Step 3: Smoke test with a real agent**

If `claude` CLI is installed and authenticated:

```bash
ION_ACP_COMMAND="claude --acp" go run ./cmd/ion/
```

If `gemini` CLI is installed:

```bash
ION_ACP_COMMAND="gemini --acp" go run ./cmd/ion/
```

Verify: ion starts, agent connects, a prompt gets a response, tool calls show in Plane B,
approval prompts work with y/n.

- [ ] **Step 4: Final commit**

```bash
git add -u
git commit -m "feat(acp): migrate to real Agent Client Protocol via coder/acp-go-sdk"
```

---

## Reference

### Key ACP types

```
acp.NewClientSideConnection(client Client, w io.Writer, r io.Reader) *ClientSideConnection
acp.NewAgentSideConnection(agent Agent, w io.Writer, r io.Reader) *AgentSideConnection

// Client methods ion must implement:
SessionUpdate(ctx, SessionNotification) error
RequestPermission(ctx, RequestPermissionRequest) (RequestPermissionResponse, error)
ReadTextFile(ctx, ReadTextFileRequest) (ReadTextFileResponse, error)
WriteTextFile(ctx, WriteTextFileRequest) (WriteTextFileResponse, error)
CreateTerminal(ctx, CreateTerminalRequest) (CreateTerminalResponse, error)
TerminalOutput(ctx, TerminalOutputRequest) (TerminalOutputResponse, error)
WaitForTerminalExit(ctx, WaitForTerminalExitRequest) (WaitForTerminalExitResponse, error)
KillTerminalCommand(ctx, KillTerminalCommandRequest) (KillTerminalCommandResponse, error)
ReleaseTerminal(ctx, ReleaseTerminalRequest) (ReleaseTerminalResponse, error)
```

### ACP agents available (via ACP Registry)

| Agent          | Command            |
| -------------- | ------------------ |
| Claude Code    | `claude --acp`     |
| Gemini CLI     | `gemini --acp`     |
| Codex CLI      | `codex --acp`      |
| GitHub Copilot | `gh copilot --acp` |

### Known gaps to address after migration

- Token usage: ACP has no standard `token_usage` event. Ion will need to estimate from model
  metadata or check if the specific agent emits `_tokenUsage` extension notifications.
- Session resume: `Resume()` currently calls `Open()`. Real continuity requires passing the
  prior session ID at connect time — check `AgentCapabilities.LoadSession` after Initialize.
- ChatGPT Plus: verify OpenAI ToS on OAuth token for direct API use. If allowed, bypass ACP
  entirely for that provider and call the API directly.

**Task:** tk-lsol
