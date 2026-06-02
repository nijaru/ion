package acp

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	acp "github.com/coder/acp-go-sdk"
	"github.com/nijaru/ion/session"
)

// mockAgent is a minimal ACP agent-side implementation for tests.
type mockAgent struct {
	conn   *acp.AgentSideConnection
	prompt func(context.Context, acp.PromptRequest) (acp.PromptResponse, error)
}

func (a *mockAgent) SetConn(c *acp.AgentSideConnection) { a.conn = c }

func (a *mockAgent) Authenticate(
	_ context.Context,
	_ acp.AuthenticateRequest,
) (acp.AuthenticateResponse, error) {
	return acp.AuthenticateResponse{}, nil
}

func (a *mockAgent) Initialize(
	_ context.Context,
	_ acp.InitializeRequest,
) (acp.InitializeResponse, error) {
	return acp.InitializeResponse{ProtocolVersion: acp.ProtocolVersionNumber}, nil
}

func (a *mockAgent) Cancel(_ context.Context, _ acp.CancelNotification) error { return nil }

func (a *mockAgent) CloseSession(
	context.Context,
	acp.CloseSessionRequest,
) (acp.CloseSessionResponse, error) {
	return acp.CloseSessionResponse{}, nil
}

func (a *mockAgent) ListSessions(
	context.Context,
	acp.ListSessionsRequest,
) (acp.ListSessionsResponse, error) {
	return acp.ListSessionsResponse{}, acp.NewMethodNotFound(acp.AgentMethodSessionList)
}

func (a *mockAgent) NewSession(
	_ context.Context,
	_ acp.NewSessionRequest,
) (acp.NewSessionResponse, error) {
	return acp.NewSessionResponse{SessionId: "test-session"}, nil
}

func (a *mockAgent) Prompt(ctx context.Context, req acp.PromptRequest) (acp.PromptResponse, error) {
	if a.prompt != nil {
		return a.prompt(ctx, req)
	}
	return acp.PromptResponse{StopReason: acp.StopReasonEndTurn}, nil
}

func (a *mockAgent) ResumeSession(
	context.Context,
	acp.ResumeSessionRequest,
) (acp.ResumeSessionResponse, error) {
	return acp.ResumeSessionResponse{}, acp.NewMethodNotFound(acp.AgentMethodSessionResume)
}

func (a *mockAgent) SetSessionConfigOption(
	context.Context,
	acp.SetSessionConfigOptionRequest,
) (acp.SetSessionConfigOptionResponse, error) {
	return acp.SetSessionConfigOptionResponse{}, acp.NewMethodNotFound(
		acp.AgentMethodSessionSetConfigOption,
	)
}

func (a *mockAgent) SetSessionMode(
	_ context.Context,
	_ acp.SetSessionModeRequest,
) (acp.SetSessionModeResponse, error) {
	return acp.SetSessionModeResponse{}, nil
}

// newTestPair creates a connected client+agent pair over in-process pipes.
// Returns the ion Session (client side) and the agent-side connection for sending notifications.
func newTestPair(t *testing.T) (*Session, *acp.AgentSideConnection) {
	t.Helper()
	client, agent, _ := newTestPairWithAgent(t)
	return client, agent
}

func newTestPairWithAgent(t *testing.T) (*Session, *acp.AgentSideConnection, *mockAgent) {
	t.Helper()

	clientRead, agentWrite := io.Pipe()
	agentRead, clientWrite := io.Pipe()

	agent := &mockAgent{}
	agentConn := acp.NewAgentSideConnection(agent, agentWrite, agentRead)
	agent.SetConn(agentConn)

	client := newSession()
	client.conn = acp.NewClientSideConnection(client, clientWrite, clientRead)
	client.sessionID = "test-session"

	ctx := context.Background()
	go func() {
		_, _ = client.conn.Initialize(ctx, acp.InitializeRequest{
			ProtocolVersion: acp.ProtocolVersionNumber,
		})
		_, _ = client.conn.NewSession(ctx, acp.NewSessionRequest{Cwd: "/tmp"})
	}()

	t.Cleanup(func() {
		_ = clientRead.Close()
		_ = clientWrite.Close()
		_ = agentRead.Close()
		_ = agentWrite.Close()
	})

	return client, agentConn, agent
}

// drainOne reads one event from the channel or fails the test after timeout.
func drainOne(t *testing.T, ch <-chan session.AgentEvent, timeout time.Duration) session.AgentEvent {
	t.Helper()
	select {
	case ev := <-ch:
		return ev
	case <-time.After(timeout):
		t.Fatal("timed out waiting for session event")
		return nil
	}
}

func TestACPSessionUpdateTextChunk(t *testing.T) {
	client, agentConn := newTestPair(t)

	ctx := context.Background()
	if err := agentConn.SessionUpdate(ctx, acp.SessionNotification{
		SessionId: "test-session",
		Update:    acp.UpdateAgentMessageText("hello"),
	}); err != nil {
		t.Fatalf("SessionUpdate: %v", err)
	}

	ev := drainOne(t, client.events, 500*time.Millisecond)
	delta, ok := ev.(session.AgentDeltaEvent)
	if !ok {
		t.Fatalf("expected AgentDelta, got %T", ev)
	}
	if delta.Delta != "hello" {
		t.Errorf("expected delta 'hello', got %q", delta.Delta)
	}
}

func TestACPSessionUpdateThought(t *testing.T) {
	client, agentConn := newTestPair(t)

	ctx := context.Background()
	if err := agentConn.SessionUpdate(ctx, acp.SessionNotification{
		SessionId: "test-session",
		Update:    acp.UpdateAgentThoughtText("thinking..."),
	}); err != nil {
		t.Fatalf("SessionUpdate: %v", err)
	}

	ev := drainOne(t, client.events, 500*time.Millisecond)
	delta, ok := ev.(session.ThinkingDeltaEvent)
	if !ok {
		t.Fatalf("expected ThinkingDelta, got %T", ev)
	}
	if delta.Delta != "thinking..." {
		t.Errorf("expected 'thinking...', got %q", delta.Delta)
	}
}

func TestACPSessionUpdateToolCall(t *testing.T) {
	client, agentConn := newTestPair(t)

	ctx := context.Background()
	if err := agentConn.SessionUpdate(ctx, acp.SessionNotification{
		SessionId: "test-session",
		Update:    acp.StartToolCall("call-1", "Read file.go"),
	}); err != nil {
		t.Fatalf("SessionUpdate: %v", err)
	}

	ev := drainOne(t, client.events, 500*time.Millisecond)
	tc, ok := ev.(session.ToolCallStartedEvent)
	if !ok {
		t.Fatalf("expected ToolCallStarted, got %T", ev)
	}
	if tc.ToolName != "Read file.go" {
		t.Errorf("expected ToolName 'Read file.go', got %q", tc.ToolName)
	}
	if tc.ToolUseID != "call-1" {
		t.Errorf("expected ToolUseID 'call-1', got %q", tc.ToolUseID)
	}
}

func TestACPSessionUpdateToolCompletion(t *testing.T) {
	client, agentConn := newTestPair(t)

	ctx := context.Background()
	// Send tool start then completion
	if err := agentConn.SessionUpdate(ctx, acp.SessionNotification{
		SessionId: "test-session",
		Update:    acp.StartToolCall("call-1", "Do something"),
	}); err != nil {
		t.Fatalf("StartToolCall: %v", err)
	}
	drainOne(t, client.events, 500*time.Millisecond) // consume ToolCallStarted

	if err := agentConn.SessionUpdate(ctx, acp.SessionNotification{
		SessionId: "test-session",
		Update:    acp.UpdateToolCall("call-1", acp.WithUpdateStatus(acp.ToolCallStatusCompleted)),
	}); err != nil {
		t.Fatalf("UpdateToolCall: %v", err)
	}

	ev := drainOne(t, client.events, 500*time.Millisecond)
	result, ok := ev.(session.ToolResultEvent)
	if !ok {
		t.Fatalf("expected ToolResult, got %T", ev)
	}
	if result.ToolUseID != "call-1" {
		t.Fatalf("tool result id = %q, want call-1", result.ToolUseID)
	}
}

func TestACPSessionUpdateTokenUsageFromNotificationMeta(t *testing.T) {
	client, _ := newTestPair(t)

	if err := client.SessionUpdate(context.Background(), acp.SessionNotification{
		SessionId: "test-session",
		Meta: map[string]any{
			"tokenUsage": map[string]any{
				"inputTokens":  12,
				"outputTokens": 3,
				"cost":         0.004,
			},
		},
	}); err != nil {
		t.Fatalf("SessionUpdate: %v", err)
	}

	ev := drainOne(t, client.events, 500*time.Millisecond)
	usage, ok := ev.(session.TokenUsageEvent)
	if !ok {
		t.Fatalf("expected TokenUsage, got %T", ev)
	}
	if usage.Input != 12 || usage.Output != 3 || usage.Cost != 0.004 {
		t.Fatalf("usage = %+v, want input 12 output 3 cost 0.004", usage)
	}
}

func TestACPSessionUpdateTokenUsageFromUpdateMeta(t *testing.T) {
	client, agentConn := newTestPair(t)

	update := acp.UpdateAgentMessageText("hello")
	update.AgentMessageChunk.Meta = map[string]any{
		"_tokenUsage": map[string]any{
			"prompt_tokens":     20,
			"completion_tokens": 5,
			"cost_usd":          "0.01",
		},
	}
	if err := agentConn.SessionUpdate(context.Background(), acp.SessionNotification{
		SessionId: "test-session",
		Update:    update,
	}); err != nil {
		t.Fatalf("SessionUpdate: %v", err)
	}

	ev := drainOne(t, client.events, 500*time.Millisecond)
	if _, ok := ev.(session.AgentDeltaEvent); !ok {
		t.Fatalf("expected AgentDelta first, got %T", ev)
	}
	ev = drainOne(t, client.events, 500*time.Millisecond)
	usage, ok := ev.(session.TokenUsageEvent)
	if !ok {
		t.Fatalf("expected TokenUsage, got %T", ev)
	}
	if usage.Input != 20 || usage.Output != 5 || usage.Cost != 0.01 {
		t.Fatalf("usage = %+v, want input 20 output 5 cost 0.01", usage)
	}
}

func TestACPApprovalBridge(t *testing.T) {
	client, agentConn := newTestPair(t)

	// Simulate agent calling RequestPermission (agent → client RPC via SDK)
	resultCh := make(chan bool, 1)
	go func() {
		resp, err := agentConn.RequestPermission(
			context.Background(),
			acp.RequestPermissionRequest{
				SessionId: "test-session",
				ToolCall: acp.ToolCallUpdate{
					ToolCallId: "call-1",
				},
				Options: []acp.PermissionOption{
					{Kind: acp.PermissionOptionKindAllowOnce, OptionId: "allow", Name: "Allow"},
					{Kind: acp.PermissionOptionKindRejectOnce, OptionId: "reject", Name: "Reject"},
				},
			},
		)
		if err != nil {
			t.Logf("RequestPermission error: %v", err)
			resultCh <- false
			return
		}
		resultCh <- resp.Outcome.Selected != nil && resp.Outcome.Selected.OptionId == "allow"
	}()

	// Wait for ApprovalRequest to arrive
	ev := drainOne(t, client.events, 500*time.Millisecond)
	req, ok := ev.(session.ApprovalRequestEvent)
	if !ok {
		t.Fatalf("expected ApprovalRequest, got %T", ev)
	}

	// Approve it
	if err := client.Approve(context.Background(), req.RequestID, true); err != nil {
		t.Fatalf("Approve: %v", err)
	}

	select {
	case approved := <-resultCh:
		if !approved {
			t.Error("expected approved=true")
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("timed out waiting for approval response")
	}
}

func TestACPFullTurn(t *testing.T) {
	// Verifies TurnStarted → TurnFinished lifecycle via the real Prompt RPC.
	// Event mapping is covered by the individual TestACPSessionUpdate* tests.
	client, _, agent := newTestPairWithAgent(t)
	agent.prompt = func(ctx context.Context, _ acp.PromptRequest) (acp.PromptResponse, error) {
		if err := agent.conn.SessionUpdate(ctx, acp.SessionNotification{
			SessionId: "test-session",
			Update:    acp.UpdateAgentMessageText("hello"),
		}); err != nil {
			return acp.PromptResponse{}, err
		}
		return acp.PromptResponse{StopReason: acp.StopReasonEndTurn}, nil
	}
	ctx := context.Background()

	if err := client.SubmitTurn(ctx, "hello"); err != nil {
		t.Fatalf("SubmitTurn: %v", err)
	}

	ev1 := drainOne(t, client.events, 500*time.Millisecond)
	if _, ok := ev1.(session.TurnStartedEvent); !ok {
		t.Fatalf("expected TurnStarted, got %T", ev1)
	}

	deadline := time.After(500 * time.Millisecond)
	for {
		select {
		case ev := <-client.events:
			if _, ok := ev.(session.TurnFinishedEvent); ok {
				return
			}
		case <-deadline:
			t.Fatal("timed out waiting for TurnFinished")
		}
	}
}

func TestACPCancelSuppressesPromptCancellationError(t *testing.T) {
	client, _, agent := newTestPairWithAgent(t)
	started := make(chan struct{})
	release := make(chan struct{})
	defer close(release)
	agent.prompt = func(ctx context.Context, _ acp.PromptRequest) (acp.PromptResponse, error) {
		close(started)
		select {
		case <-ctx.Done():
			return acp.PromptResponse{}, ctx.Err()
		case <-release:
			return acp.PromptResponse{StopReason: acp.StopReasonEndTurn}, nil
		}
	}

	if err := client.SubmitTurn(t.Context(), "hello"); err != nil {
		t.Fatalf("SubmitTurn: %v", err)
	}
	if _, ok := drainOne(t, client.events, 500*time.Millisecond).(session.TurnStartedEvent); !ok {
		t.Fatal("expected TurnStarted")
	}
	select {
	case <-started:
	case <-time.After(500 * time.Millisecond):
		t.Fatal("timed out waiting for ACP prompt")
	}
	if err := client.CancelTurn(t.Context()); err != nil {
		t.Fatalf("CancelTurn: %v", err)
	}

	deadline := time.After(500 * time.Millisecond)
	for {
		select {
		case ev := <-client.events:
			switch msg := ev.(type) {
			case session.ErrorEvent:
				t.Fatalf("cancel emitted prompt error: %v", msg.Err)
			case session.TurnFinishedEvent:
				return
			}
		case <-deadline:
			t.Fatal("timed out waiting for TurnFinished after cancel")
		}
	}
}

func TestACPCloseDuringPromptClosesEventsWithoutLateSend(t *testing.T) {
	client, _, agent := newTestPairWithAgent(t)
	started := make(chan struct{})
	agent.prompt = func(ctx context.Context, _ acp.PromptRequest) (acp.PromptResponse, error) {
		close(started)
		<-ctx.Done()
		return acp.PromptResponse{}, ctx.Err()
	}

	if err := client.SubmitTurn(context.Background(), "hello"); err != nil {
		t.Fatalf("SubmitTurn: %v", err)
	}
	if _, ok := drainOne(t, client.events, 500*time.Millisecond).(session.TurnStartedEvent); !ok {
		t.Fatal("expected TurnStarted")
	}
	select {
	case <-started:
	case <-time.After(500 * time.Millisecond):
		t.Fatal("timed out waiting for ACP prompt")
	}

	done := make(chan error, 1)
	go func() { done <- client.Close() }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("Close blocked while prompt was active")
	}

	if _, ok := <-client.events; ok {
		t.Fatal("events channel remained open after Close")
	}
}

func TestACPRejectsConcurrentSubmit(t *testing.T) {
	client, _, agent := newTestPairWithAgent(t)
	release := make(chan struct{})
	defer close(release)
	agent.prompt = func(ctx context.Context, _ acp.PromptRequest) (acp.PromptResponse, error) {
		select {
		case <-ctx.Done():
			return acp.PromptResponse{}, ctx.Err()
		case <-release:
			return acp.PromptResponse{StopReason: acp.StopReasonEndTurn}, nil
		}
	}

	if err := client.SubmitTurn(t.Context(), "first"); err != nil {
		t.Fatalf("SubmitTurn first: %v", err)
	}
	if _, ok := drainOne(t, client.events, 500*time.Millisecond).(session.TurnStartedEvent); !ok {
		t.Fatal("expected TurnStarted")
	}
	if err := client.SubmitTurn(t.Context(), "second"); err == nil ||
		!strings.Contains(err.Error(), "turn already in progress") {
		t.Fatalf("SubmitTurn second error = %v, want in-progress error", err)
	}
}

func TestACPRequestPermissionCancelRemovesPendingApproval(t *testing.T) {
	client := newSession()
	ctx, cancel := context.WithCancel(t.Context())
	result := make(chan error, 1)
	go func() {
		_, err := client.RequestPermission(ctx, acp.RequestPermissionRequest{
			SessionId: "test-session",
			ToolCall:  acp.ToolCallUpdate{ToolCallId: "call-1"},
			Options: []acp.PermissionOption{
				{Kind: acp.PermissionOptionKindAllowOnce, OptionId: "allow", Name: "Allow"},
				{Kind: acp.PermissionOptionKindRejectOnce, OptionId: "reject", Name: "Reject"},
			},
		})
		result <- err
	}()

	ev := drainOne(t, client.events, 500*time.Millisecond)
	if _, ok := ev.(session.ApprovalRequestEvent); !ok {
		t.Fatalf("expected ApprovalRequest, got %T", ev)
	}
	client.mu.Lock()
	pending := len(client.pendingApprovals)
	client.mu.Unlock()
	if pending != 1 {
		t.Fatalf("pending approvals = %d, want 1", pending)
	}

	cancel()
	select {
	case err := <-result:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("RequestPermission error = %v, want context canceled", err)
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("timed out waiting for canceled permission request")
	}
	client.mu.Lock()
	pending = len(client.pendingApprovals)
	client.mu.Unlock()
	if pending != 0 {
		t.Fatalf("pending approvals = %d, want cleaned up", pending)
	}
}

func TestACPSessionRequestIncludesInitialContext(t *testing.T) {
	cwd := t.TempDir()
	if err := os.WriteFile(filepath.Join(cwd, "AGENTS.md"), []byte("project instruction"), 0o644); err != nil {
		t.Fatalf("write AGENTS.md: %v", err)
	}

	stor := session.NewLazySession(nil, cwd, "openrouter/model-a", "feature/acp")
	client := newSession()
	client.storage = stor
	client.resumeSessionID = "external-session-123"

	req, err := client.newSessionRequest()
	if err != nil {
		t.Fatalf("newSessionRequest: %v", err)
	}

	if req.Cwd != cwd {
		t.Fatalf("cwd = %q, want %q", req.Cwd, cwd)
	}
	if req.McpServers == nil {
		t.Fatal("mcpServers must be an explicit empty list")
	}
	meta, ok := req.Meta["ion"].(ionSessionContext)
	if !ok {
		t.Fatalf("meta ion = %T, want ionSessionContext", req.Meta["ion"])
	}
	if meta.SessionID != stor.ID() {
		t.Fatalf("session id = %q, want %q", meta.SessionID, stor.ID())
	}
	if meta.CWD != cwd {
		t.Fatalf("meta cwd = %q, want %q", meta.CWD, cwd)
	}
	if meta.Branch != "feature/acp" {
		t.Fatalf("branch = %q, want feature/acp", meta.Branch)
	}
	if meta.Model != "openrouter/model-a" {
		t.Fatalf("model = %q, want openrouter/model-a", meta.Model)
	}
	if meta.ResumeSession != "external-session-123" {
		t.Fatalf("resume session = %q, want external-session-123", meta.ResumeSession)
	}
	if !strings.Contains(meta.SystemPrompt, "<project_context>") ||
		!strings.Contains(meta.SystemPrompt, "project instruction") {
		t.Fatalf("system prompt missing project instructions: %q", meta.SystemPrompt)
	}
}

func TestACPSessionRequestNormalizesRelativeCWD(t *testing.T) {
	client := newSession()
	client.storage = session.NewLazySession(nil, ".", "model-a", "main")

	req, err := client.newSessionRequest()
	if err != nil {
		t.Fatalf("newSessionRequest: %v", err)
	}
	if !filepath.IsAbs(req.Cwd) {
		t.Fatalf("cwd = %q, want absolute path", req.Cwd)
	}
	if meta := req.Meta["ion"].(ionSessionContext); meta.CWD != req.Cwd {
		t.Fatalf("meta cwd = %q, want request cwd %q", meta.CWD, req.Cwd)
	}
}

func TestACPFileBridgeResolvesRelativePathsAgainstSessionCWD(t *testing.T) {
	processCWD := t.TempDir()
	t.Chdir(processCWD)

	cwd := t.TempDir()
	if err := os.WriteFile(filepath.Join(cwd, "input.txt"), []byte("one\ntwo\nthree\n"), 0o644); err != nil {
		t.Fatalf("write input: %v", err)
	}

	client := newSession()
	client.storage = session.NewLazySession(nil, cwd, "model-a", "main")

	line := 2
	limit := 1
	resp, err := client.ReadTextFile(t.Context(), acp.ReadTextFileRequest{
		Path:  "input.txt",
		Line:  &line,
		Limit: &limit,
	})
	if err != nil {
		t.Fatalf("ReadTextFile: %v", err)
	}
	if resp.Content != "two\n" {
		t.Fatalf("content = %q, want second line", resp.Content)
	}

	if _, err := client.WriteTextFile(t.Context(), acp.WriteTextFileRequest{
		Path:    "output.txt",
		Content: "written",
	}); err != nil {
		t.Fatalf("WriteTextFile: %v", err)
	}
	data, err := os.ReadFile(filepath.Join(cwd, "output.txt"))
	if err != nil {
		t.Fatalf("read workspace output: %v", err)
	}
	if string(data) != "written" {
		t.Fatalf("workspace output = %q, want written", data)
	}
	if _, err := os.Stat(filepath.Join(processCWD, "output.txt")); !os.IsNotExist(err) {
		t.Fatalf("relative write escaped to process cwd, stat err = %v", err)
	}
}

func TestACPFileBridgeRejectsEscapingPaths(t *testing.T) {
	cwd := t.TempDir()
	client := newSession()
	client.storage = session.NewLazySession(nil, cwd, "model-a", "main")
	client.ctx = t.Context()

	if _, err := client.ReadTextFile(t.Context(), acp.ReadTextFileRequest{Path: "../outside.txt"}); err == nil {
		t.Fatal("ReadTextFile accepted path outside workspace")
	}

	outside := ".."
	if _, err := client.CreateTerminal(t.Context(), acp.CreateTerminalRequest{
		Command: "true",
		Cwd:     &outside,
	}); err == nil {
		t.Fatal("CreateTerminal accepted cwd outside workspace")
	}
}

func TestACPTerminalOutputHonorsByteLimit(t *testing.T) {
	client := newSession()
	term := &terminal{
		done:           make(chan struct{}),
		outputLimit:    5,
		hasOutputLimit: true,
	}
	close(term.done)
	if _, err := term.Write([]byte("abcdeé")); err != nil {
		t.Fatalf("terminal write: %v", err)
	}
	client.terminals["term-1"] = term

	resp, err := client.TerminalOutput(t.Context(), acp.TerminalOutputRequest{TerminalId: "term-1"})
	if err != nil {
		t.Fatalf("TerminalOutput: %v", err)
	}
	if resp.Output != "cdeé" {
		t.Fatalf("output = %q, want suffix within byte limit", resp.Output)
	}
	if !resp.Truncated {
		t.Fatal("TerminalOutput did not report truncation")
	}

	resp, err = client.TerminalOutput(t.Context(), acp.TerminalOutputRequest{TerminalId: "term-1"})
	if err != nil {
		t.Fatalf("TerminalOutput second read: %v", err)
	}
	if resp.Output != "" || resp.Truncated {
		t.Fatalf(
			"second output = (%q, truncated=%v), want empty non-truncated",
			resp.Output,
			resp.Truncated,
		)
	}
}

func TestACPCommandEnvIncludesResumeSessionID(t *testing.T) {
	env := acpCommandEnv("session-123")

	found := false
	for _, value := range env {
		if value == "ION_ACP_SESSION_ID=session-123" {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("expected resume session ID in env, got %v", env)
	}
}

func TestACPStderrWriterDefaultsToDiscard(t *testing.T) {
	t.Setenv("ION_ACP_STDERR_LOG", "")

	w, cleanup, err := acpStderrWriter()
	if err != nil {
		t.Fatalf("acpStderrWriter returned error: %v", err)
	}
	if _, err := io.WriteString(w, "agent warning token=sk-test1234567890\n"); err != nil {
		t.Fatalf("write stderr: %v", err)
	}
	if err := cleanup(); err != nil {
		t.Fatalf("cleanup stderr: %v", err)
	}
}

func TestACPStderrWriterAppendsToDebugLog(t *testing.T) {
	path := filepath.Join(t.TempDir(), "acp-stderr.log")
	t.Setenv("ION_ACP_STDERR_LOG", path)

	w, cleanup, err := acpStderrWriter()
	if err != nil {
		t.Fatalf("acpStderrWriter returned error: %v", err)
	}
	if _, err := io.WriteString(w, "agent warning token=sk-test1234567890\n"); err != nil {
		t.Fatalf("write stderr: %v", err)
	}
	if err := cleanup(); err != nil {
		t.Fatalf("cleanup stderr: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read stderr log: %v", err)
	}
	text := string(data)
	if !strings.Contains(text, "agent warning") {
		t.Fatalf("stderr log = %q, want warning", data)
	}
	if strings.Contains(text, "sk-test1234567890") {
		t.Fatalf("stderr log leaked token: %q", data)
	}
	if !strings.Contains(text, "[redacted-secret]") {
		t.Fatalf("stderr log missing redaction marker: %q", data)
	}
}
