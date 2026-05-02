package main

import (
	"context"
	"io"
	"slices"
	"sync"
	"testing"
	"time"

	acp "github.com/coder/acp-go-sdk"
	ionsession "github.com/nijaru/ion/internal/session"
)

type acpTestClient struct {
	mu      sync.Mutex
	updates []acp.SessionNotification
}

func (c *acpTestClient) WriteTextFile(
	context.Context,
	acp.WriteTextFileRequest,
) (acp.WriteTextFileResponse, error) {
	return acp.WriteTextFileResponse{}, nil
}

func (c *acpTestClient) ReadTextFile(
	context.Context,
	acp.ReadTextFileRequest,
) (acp.ReadTextFileResponse, error) {
	return acp.ReadTextFileResponse{}, nil
}

func (c *acpTestClient) RequestPermission(
	context.Context,
	acp.RequestPermissionRequest,
) (acp.RequestPermissionResponse, error) {
	return acp.RequestPermissionResponse{
		Outcome: acp.RequestPermissionOutcome{
			Selected: &acp.RequestPermissionOutcomeSelected{
				OptionId: acp.PermissionOptionId("allow"),
			},
		},
	}, nil
}

func (c *acpTestClient) SessionUpdate(
	_ context.Context,
	n acp.SessionNotification,
) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.updates = append(c.updates, n)
	return nil
}

func (c *acpTestClient) CreateTerminal(
	context.Context,
	acp.CreateTerminalRequest,
) (acp.CreateTerminalResponse, error) {
	return acp.CreateTerminalResponse{}, nil
}

func (c *acpTestClient) KillTerminalCommand(
	context.Context,
	acp.KillTerminalCommandRequest,
) (acp.KillTerminalCommandResponse, error) {
	return acp.KillTerminalCommandResponse{}, nil
}

func (c *acpTestClient) ReleaseTerminal(
	context.Context,
	acp.ReleaseTerminalRequest,
) (acp.ReleaseTerminalResponse, error) {
	return acp.ReleaseTerminalResponse{}, nil
}

func (c *acpTestClient) TerminalOutput(
	context.Context,
	acp.TerminalOutputRequest,
) (acp.TerminalOutputResponse, error) {
	return acp.TerminalOutputResponse{}, nil
}

func (c *acpTestClient) WaitForTerminalExit(
	context.Context,
	acp.WaitForTerminalExitRequest,
) (acp.WaitForTerminalExitResponse, error) {
	return acp.WaitForTerminalExitResponse{}, nil
}

func (c *acpTestClient) snapshot() []acp.SessionNotification {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.updates)
}

type fakeACPRuntimeFactory struct {
	session *fakeACPAgentSession
	cwd     string
}

func (f *fakeACPRuntimeFactory) Open(
	_ context.Context,
	cwd string,
	_ string,
) (ionsession.AgentSession, func() error, error) {
	f.cwd = cwd
	return f.session, func() error { return f.session.Close() }, nil
}

type fakeACPAgentSession struct {
	id        string
	events    chan ionsession.Event
	submitted chan string
	canceled  chan struct{}
	closed    chan struct{}
	script    []ionsession.Event

	mu           sync.Mutex
	mode         ionsession.Mode
	autoApproved bool
	approvals    []bool
}

func newFakeACPAgentSession(id string, script ...ionsession.Event) *fakeACPAgentSession {
	return &fakeACPAgentSession{
		id:        id,
		events:    make(chan ionsession.Event, 16),
		submitted: make(chan string, 1),
		canceled:  make(chan struct{}, 1),
		closed:    make(chan struct{}),
		script:    script,
	}
}

func (s *fakeACPAgentSession) Open(context.Context) error { return nil }

func (s *fakeACPAgentSession) Resume(context.Context, string) error { return nil }

func (s *fakeACPAgentSession) SubmitTurn(_ context.Context, turn string) error {
	s.submitted <- turn
	go func() {
		for _, event := range s.script {
			s.events <- event
		}
	}()
	return nil
}

func (s *fakeACPAgentSession) CancelTurn(context.Context) error {
	select {
	case s.canceled <- struct{}{}:
	default:
	}
	return nil
}

func (s *fakeACPAgentSession) Approve(_ context.Context, _ string, approved bool) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.approvals = append(s.approvals, approved)
	return nil
}

func (s *fakeACPAgentSession) RegisterMCPServer(context.Context, string, ...string) error {
	return nil
}

func (s *fakeACPAgentSession) SetMode(mode ionsession.Mode) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.mode = mode
}

func (s *fakeACPAgentSession) SetAutoApprove(enabled bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.autoApproved = enabled
}

func (s *fakeACPAgentSession) AllowCategory(string) {}

func (s *fakeACPAgentSession) Close() error {
	select {
	case <-s.closed:
	default:
		close(s.closed)
	}
	return nil
}

func (s *fakeACPAgentSession) Events() <-chan ionsession.Event { return s.events }

func (s *fakeACPAgentSession) ID() string { return s.id }

func (s *fakeACPAgentSession) Meta() map[string]string { return nil }

func connectACPAgent(t *testing.T, agent *ionACPAgent) *acp.ClientSideConnection {
	t.Helper()
	a2cR, a2cW := io.Pipe()
	c2aR, c2aW := io.Pipe()
	t.Cleanup(func() {
		_ = a2cR.Close()
		_ = a2cW.Close()
		_ = c2aR.Close()
		_ = c2aW.Close()
	})

	agentConn := acp.NewAgentSideConnection(agent, a2cW, c2aR)
	agent.SetAgentConnection(agentConn)
	return acp.NewClientSideConnection(&acpTestClient{}, c2aW, a2cR)
}

func TestIonACPAgentStreamsSessionUpdates(t *testing.T) {
	workdir := t.TempDir()
	readPath := workdir + "/README.md"
	fakeSession := newFakeACPAgentSession(
		"session-1",
		ionsession.TurnStarted{},
		ionsession.AgentDelta{Delta: "hello"},
		ionsession.ThinkingDelta{Delta: "thinking"},
		ionsession.ToolCallStarted{
			ToolUseID: "tool-1",
			ToolName:  "read",
			Args:      `{"file_path":"` + readPath + `"}`,
		},
		ionsession.ToolResult{ToolUseID: "tool-1", Result: "file contents"},
		ionsession.TurnFinished{},
	)
	factory := &fakeACPRuntimeFactory{session: fakeSession}
	agent := newIonACPAgent(factory, "test-version", ionsession.ModeEdit)

	a2cR, a2cW := io.Pipe()
	c2aR, c2aW := io.Pipe()
	t.Cleanup(func() {
		_ = a2cR.Close()
		_ = a2cW.Close()
		_ = c2aR.Close()
		_ = c2aW.Close()
	})

	agentConn := acp.NewAgentSideConnection(agent, a2cW, c2aR)
	agent.SetAgentConnection(agentConn)
	client := &acpTestClient{}
	clientConn := acp.NewClientSideConnection(client, c2aW, a2cR)

	initResp, err := clientConn.Initialize(t.Context(), acp.InitializeRequest{
		ProtocolVersion: acp.ProtocolVersionNumber,
	})
	if err != nil {
		t.Fatalf("initialize: %v", err)
	}
	if initResp.AgentInfo == nil || initResp.AgentInfo.Name != "ion" {
		t.Fatalf("agent info = %#v, want ion", initResp.AgentInfo)
	}
	if !initResp.AgentCapabilities.LoadSession {
		t.Fatalf("agent capabilities = %#v, want load session", initResp.AgentCapabilities)
	}

	newResp, err := clientConn.NewSession(t.Context(), acp.NewSessionRequest{
		Cwd:        workdir,
		McpServers: []acp.McpServer{},
	})
	if err != nil {
		t.Fatalf("new session: %v", err)
	}
	if got := string(newResp.SessionId); got != "session-1" {
		t.Fatalf("session id = %q, want session-1", got)
	}
	if factory.cwd != workdir {
		t.Fatalf("factory cwd = %q, want %q", factory.cwd, workdir)
	}

	promptResp, err := clientConn.Prompt(t.Context(), acp.PromptRequest{
		SessionId: newResp.SessionId,
		Prompt: []acp.ContentBlock{
			acp.TextBlock("read this"),
			acp.ResourceLinkBlock("README", "file://README.md"),
		},
	})
	if err != nil {
		t.Fatalf("prompt: %v", err)
	}
	if promptResp.StopReason != acp.StopReasonEndTurn {
		t.Fatalf("stop reason = %q, want end_turn", promptResp.StopReason)
	}

	select {
	case submitted := <-fakeSession.submitted:
		want := "read this\n\nREADME: file://README.md"
		if submitted != want {
			t.Fatalf("submitted prompt = %q, want %q", submitted, want)
		}
	default:
		t.Fatalf("fake session did not receive submitted prompt")
	}

	updates := waitForACPUpdates(t, client, 4)
	if len(updates) != 4 {
		t.Fatalf("updates len = %d, want 4: %#v", len(updates), updates)
	}
	if updates[0].Update.AgentMessageChunk == nil ||
		updates[0].Update.AgentMessageChunk.Content.Text.Text != "hello" {
		t.Fatalf("first update = %#v, want agent message", updates[0].Update)
	}
	if updates[1].Update.AgentThoughtChunk == nil ||
		updates[1].Update.AgentThoughtChunk.Content.Text.Text != "thinking" {
		t.Fatalf("second update = %#v, want thought", updates[1].Update)
	}
	if updates[2].Update.ToolCall == nil ||
		updates[2].Update.ToolCall.Title != "Read(README.md)" ||
		updates[2].Update.ToolCall.Kind != acp.ToolKindRead {
		t.Fatalf("third update = %#v, want read tool call", updates[2].Update)
	}
	if updates[3].Update.ToolCallUpdate == nil ||
		updates[3].Update.ToolCallUpdate.Status == nil ||
		*updates[3].Update.ToolCallUpdate.Status != acp.ToolCallStatusCompleted {
		t.Fatalf("fourth update = %#v, want completed tool result", updates[3].Update)
	}
}

func TestIonACPAgentSetSessionMode(t *testing.T) {
	fakeSession := newFakeACPAgentSession("session-1")
	agent := newIonACPAgent(
		&fakeACPRuntimeFactory{session: fakeSession},
		"test-version",
		ionsession.ModeRead,
	)
	clientConn := connectACPAgent(t, agent)

	newResp, err := clientConn.NewSession(t.Context(), acp.NewSessionRequest{
		Cwd:        t.TempDir(),
		McpServers: []acp.McpServer{},
	})
	if err != nil {
		t.Fatalf("new session: %v", err)
	}
	if newResp.Modes == nil || newResp.Modes.CurrentModeId != "read" {
		t.Fatalf("initial modes = %#v, want read", newResp.Modes)
	}

	if _, err := clientConn.SetSessionMode(t.Context(), acp.SetSessionModeRequest{
		SessionId: newResp.SessionId,
		ModeId:    acp.SessionModeId("auto"),
	}); err != nil {
		t.Fatalf("set session mode: %v", err)
	}
	fakeSession.mu.Lock()
	defer fakeSession.mu.Unlock()
	if fakeSession.mode != ionsession.ModeYolo || !fakeSession.autoApproved {
		t.Fatalf("mode = %v auto = %v, want yolo auto", fakeSession.mode, fakeSession.autoApproved)
	}
}

func TestACPPromptTextRejectsUnsupportedBlocks(t *testing.T) {
	_, err := acpPromptText([]acp.ContentBlock{
		acp.ImageBlock("abc", "image/png"),
	})
	if err == nil {
		t.Fatalf("acpPromptText accepted image block")
	}
}

func waitForACPUpdates(
	t *testing.T,
	client *acpTestClient,
	count int,
) []acp.SessionNotification {
	t.Helper()
	deadline := time.Now().Add(time.Second)
	for time.Now().Before(deadline) {
		updates := client.snapshot()
		if len(updates) >= count {
			return updates
		}
		time.Sleep(10 * time.Millisecond)
	}
	return client.snapshot()
}
