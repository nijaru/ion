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
func drainOne(t *testing.T, ch <-chan session.Event, timeout time.Duration) session.Event {
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
	delta, ok := ev.(session.AgentDelta)
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
	delta, ok := ev.(session.ThinkingDelta)
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
	tc, ok := ev.(session.ToolCallStarted)
	if !ok {
		t.Fatalf("expected ToolCallStarted, got %T", ev)
	}
	if tc.ToolName != "Read file.go" {
		t.Errorf("expected ToolName 'Read file.go', got %q", tc.ToolName)
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
	if _, ok := ev.(session.ToolResult); !ok {
		t.Fatalf("expected ToolResult, got %T", ev)
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
				ToolCall: acp.RequestPermissionToolCall{
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
	req, ok := ev.(session.ApprovalRequest)
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
	if _, ok := ev1.(session.TurnStarted); !ok {
		t.Fatalf("expected TurnStarted, got %T", ev1)
	}

	ev2 := drainOne(t, client.events, 500*time.Millisecond)
	if _, ok := ev2.(session.AgentDelta); !ok {
		t.Fatalf("expected AgentDelta, got %T", ev2)
	}

	ev3 := drainOne(t, client.events, 500*time.Millisecond)
	if _, ok := ev3.(session.AgentMessage); !ok {
		t.Fatalf("expected AgentMessage, got %T", ev3)
	}

	ev4 := drainOne(t, client.events, 500*time.Millisecond)
	if _, ok := ev4.(session.TurnFinished); !ok {
		t.Fatalf("expected TurnFinished, got %T", ev4)
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
