package acp

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"time"

	acp "github.com/coder/acp-go-sdk"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/privacy"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// terminal tracks a running process created by CreateTerminal.
type terminal struct {
	mu       sync.Mutex
	buf      bytes.Buffer
	cmd      *exec.Cmd
	exitCode *int
	done     chan struct{}
}

// Write implements io.Writer — called from the copy goroutine.
func (t *terminal) Write(p []byte) (int, error) {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.buf.Write(p)
}

// Session is ion's ACP client session. It spawns an external agent process,
// connects via JSON-RPC 2.0 over stdio, and translates ACP events to session.Event.
type Session struct {
	events  chan session.Event
	store   storage.Store
	storage storage.Session
	policy  *backend.PolicyEngine

	conn            *acp.ClientSideConnection
	sessionID       string
	cmd             *exec.Cmd
	ctx             context.Context
	cancel          context.CancelFunc
	closeOnce       sync.Once
	mu              sync.Mutex
	resumeSessionID string
	stderrCleanup   func() error

	// Pending approval requests: requestID → response channel
	pendingApprovals map[string]chan bool
	// Running terminals: terminalID → terminal
	terminals map[string]*terminal
}

func newSession() *Session {
	return &Session{
		events:           make(chan session.Event, 100),
		policy:           backend.NewPolicyEngine(),
		pendingApprovals: make(map[string]chan bool),
		terminals:        make(map[string]*terminal),
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
	s.ctx = ctx
	s.cancel = cancel

	cmd := exec.CommandContext(ctx, "bash", "-c", command)
	cmd.Env = acpCommandEnv(s.resumeSessionID)
	s.cmd = cmd
	s.resumeSessionID = ""

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
	stderr, cleanupStderr, err := acpStderrWriter()
	if err != nil {
		cancel()
		return err
	}
	s.stderrCleanup = cleanupStderr
	cmd.Stderr = stderr

	if err := cmd.Start(); err != nil {
		cancel()
		_ = cleanupStderr()
		return fmt.Errorf("start agent: %w", err)
	}

	s.conn = acp.NewClientSideConnection(s, stdin, stdout)

	_, err = s.conn.Initialize(ctx, acp.InitializeRequest{
		ProtocolVersion: acp.ProtocolVersionNumber,
		ClientInfo: &acp.Implementation{
			Name:    "ion",
			Version: "v0.0.0",
		},
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
		_ = cleanupStderr()
		return fmt.Errorf("acp initialize: %w", err)
	}

	req, err := s.newSessionRequest()
	if err != nil {
		cancel()
		_ = cleanupStderr()
		return fmt.Errorf("acp session context: %w", err)
	}
	resp, err := s.conn.NewSession(ctx, req)
	if err != nil {
		cancel()
		_ = cleanupStderr()
		return fmt.Errorf("acp new session: %w", err)
	}
	s.sessionID = string(resp.SessionId)

	go func() { _ = cmd.Wait() }()

	return nil
}

type ionSessionMeta struct {
	Ion ionSessionContext `json:"ion"`
}

type ionSessionContext struct {
	SessionID     string `json:"sessionId,omitempty"`
	CWD           string `json:"cwd"`
	Branch        string `json:"branch,omitempty"`
	Model         string `json:"model,omitempty"`
	SystemPrompt  string `json:"systemPrompt,omitempty"`
	ResumeSession string `json:"resumeSession,omitempty"`
}

func (s *Session) newSessionRequest() (acp.NewSessionRequest, error) {
	cwd := cwdFromStorage(s.storage)
	instructions, err := backend.BuildInstructions("", cwd)
	if err != nil {
		return acp.NewSessionRequest{}, err
	}

	ctx := ionSessionContext{
		CWD:           cwd,
		SystemPrompt:  strings.TrimSpace(instructions),
		ResumeSession: strings.TrimSpace(s.resumeSessionID),
	}
	if s.storage != nil {
		meta := s.storage.Meta()
		ctx.SessionID = strings.TrimSpace(meta.ID)
		ctx.Branch = strings.TrimSpace(meta.Branch)
		ctx.Model = strings.TrimSpace(meta.Model)
	}

	return acp.NewSessionRequest{
		Cwd:        cwd,
		McpServers: []acp.McpServer{},
		Meta:       ionSessionMeta{Ion: ctx},
	}, nil
}

func (s *Session) Resume(ctx context.Context, sessionID string) error {
	s.mu.Lock()
	s.resumeSessionID = sessionID
	s.mu.Unlock()
	return s.Open(ctx)
}

func (s *Session) SetMode(mode session.Mode) {
	s.policy.SetMode(mode)
}

func (s *Session) SetAutoApprove(enabled bool) {
	s.policy.SetAutoApprove(enabled)
}

func (s *Session) AllowCategory(toolName string) {
	s.policy.AllowCategoryOf(toolName)
}

func (s *Session) Close() error {
	s.closeOnce.Do(func() {
		if s.cancel != nil {
			s.cancel()
		}
		if s.stderrCleanup != nil {
			_ = s.stderrCleanup()
			s.stderrCleanup = nil
		}

		// Cleanup terminals
		s.mu.Lock()
		for id, t := range s.terminals {
			if t.cmd != nil && t.cmd.Process != nil {
				_ = t.cmd.Process.Kill()
			}
			delete(s.terminals, id)
		}
		s.mu.Unlock()

		close(s.events)
	})
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

// SubmitTurn sends user input to the agent. Prompt runs in a goroutine;
// TurnStarted is emitted immediately, TurnFinished after Prompt returns.
func (s *Session) SubmitTurn(ctx context.Context, input string) error {
	s.mu.Lock()
	conn := s.conn
	sessionID := s.sessionID
	s.mu.Unlock()

	if conn == nil {
		return fmt.Errorf("not connected")
	}

	s.events <- session.TurnStarted{}

	go func() {
		_, err := conn.Prompt(ctx, acp.PromptRequest{
			SessionId: acp.SessionId(sessionID),
			Prompt:    []acp.ContentBlock{acp.TextBlock(input)},
		})
		if err != nil {
			s.events <- session.Error{Err: fmt.Errorf("prompt: %w", err)}
		}
		s.events <- session.TurnFinished{}
	}()

	return nil
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

// Approve resolves a pending RequestPermission call.
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

func acpCommandEnv(sessionID string) []string {
	env := os.Environ()
	if sessionID != "" {
		env = append(env, "ION_ACP_SESSION_ID="+sessionID)
	}
	return env
}

func acpStderrWriter() (io.Writer, func() error, error) {
	path := strings.TrimSpace(os.Getenv("ION_ACP_STDERR_LOG"))
	if path == "" {
		return io.Discard, func() error { return nil }, nil
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return nil, nil, fmt.Errorf("open ACP stderr log: %w", err)
	}
	return redactingWriter{dst: f}, f.Close, nil
}

type redactingWriter struct {
	dst io.Writer
}

func (w redactingWriter) Write(p []byte) (int, error) {
	if _, err := io.WriteString(w.dst, privacy.Redact(string(p))); err != nil {
		return 0, err
	}
	return len(p), nil
}

// SessionUpdate implements acp.Client — translates ACP notifications to session.Event.
func (s *Session) SessionUpdate(ctx context.Context, n acp.SessionNotification) error {
	update := n.Update
	usage, hasUsage := tokenUsageFromNotification(n)

	switch {
	case update.AgentMessageChunk != nil:
		if update.AgentMessageChunk.Content.Text != nil {
			s.events <- session.AgentDelta{Delta: update.AgentMessageChunk.Content.Text.Text}
		}

	case update.AgentThoughtChunk != nil:
		if update.AgentThoughtChunk.Content.Text != nil {
			s.events <- session.ThinkingDelta{Delta: update.AgentThoughtChunk.Content.Text.Text}
		}

	case update.ToolCall != nil:
		tc := update.ToolCall
		toolName := tc.Title
		if toolName == "" {
			toolName = string(tc.Kind)
		}
		args := ""
		if tc.RawInput != nil {
			args = fmt.Sprintf("%v", tc.RawInput)
		}
		s.events <- session.ToolCallStarted{
			ToolUseID: string(tc.ToolCallId),
			ToolName:  toolName,
			Args:      args,
		}

	case update.ToolCallUpdate != nil:
		tcu := update.ToolCallUpdate
		switch {
		case tcu.Status != nil && *tcu.Status == acp.ToolCallStatusCompleted:
			output := toolContentText(tcu.Content)
			if output == "" && tcu.RawOutput != nil {
				output = fmt.Sprintf("%v", tcu.RawOutput)
			}
			s.events <- session.ToolResult{ToolUseID: string(tcu.ToolCallId), Result: output}

		case tcu.Status != nil && *tcu.Status == acp.ToolCallStatusFailed:
			output := toolContentText(tcu.Content)
			s.events <- session.ToolResult{
				ToolUseID: string(tcu.ToolCallId),
				Result:    output,
				Error:     fmt.Errorf("tool call failed"),
			}

		default:
			if delta := toolContentText(tcu.Content); delta != "" {
				s.events <- session.ToolOutputDelta{ToolUseID: string(tcu.ToolCallId), Delta: delta}
			}
		}

	case update.Plan != nil:
		if len(update.Plan.Entries) > 0 {
			s.events <- session.StatusChanged{Status: update.Plan.Entries[0].Content}
		}
	}

	if hasUsage {
		s.events <- usage
	}

	return nil
}

// RequestPermission implements acp.Client — blocks until the user approves or denies.
func (s *Session) RequestPermission(
	ctx context.Context,
	p acp.RequestPermissionRequest,
) (acp.RequestPermissionResponse, error) {
	requestID := string(p.ToolCall.ToolCallId)

	// Determine a display name from kind or title
	toolName := ""
	if p.ToolCall.Kind != nil {
		toolName = string(*p.ToolCall.Kind)
	}
	if p.ToolCall.Title != nil {
		toolName = *p.ToolCall.Title
	}

	// Policy engine may auto-approve or auto-deny
	policy, _ := s.policy.Authorize(ctx, toolName, "")
	switch policy {
	case backend.PolicyAllow:
		return allowResponse(p), nil
	case backend.PolicyDeny:
		return denyResponse(p), nil
	}

	// Ask the user via the TUI
	ch := make(chan bool, 1)
	s.mu.Lock()
	s.pendingApprovals[requestID] = ch
	s.mu.Unlock()

	s.events <- session.ApprovalRequest{
		RequestID:   requestID,
		ToolName:    toolName,
		Description: toolName,
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

// validatePath ensures the given path is within the workspace CWD.
func (s *Session) validatePath(path string) error {
	m := s.Meta()
	if m == nil || m["cwd"] == "" {
		return fmt.Errorf("session CWD not available for validation")
	}
	cwd := m["cwd"]

	absPath, err := filepath.Abs(path)
	if err != nil {
		return err
	}
	absCwd, err := filepath.Abs(cwd)
	if err != nil {
		return err
	}

	if !strings.HasPrefix(absPath, absCwd+string(filepath.Separator)) && absPath != absCwd {
		return fmt.Errorf("path escapes workspace: %s", path)
	}
	return nil
}

// ReadTextFile implements acp.Client.
func (s *Session) ReadTextFile(
	_ context.Context,
	p acp.ReadTextFileRequest,
) (acp.ReadTextFileResponse, error) {
	if err := s.validatePath(p.Path); err != nil {
		return acp.ReadTextFileResponse{}, err
	}
	data, err := os.ReadFile(p.Path)
	if err != nil {
		return acp.ReadTextFileResponse{}, fmt.Errorf("read %s: %w", p.Path, err)
	}
	return acp.ReadTextFileResponse{Content: string(data)}, nil
}

// WriteTextFile implements acp.Client. Permission was already granted via RequestPermission.
func (s *Session) WriteTextFile(
	_ context.Context,
	p acp.WriteTextFileRequest,
) (acp.WriteTextFileResponse, error) {
	if err := s.validatePath(p.Path); err != nil {
		return acp.WriteTextFileResponse{}, err
	}
	if err := os.WriteFile(p.Path, []byte(p.Content), 0o644); err != nil {
		return acp.WriteTextFileResponse{}, fmt.Errorf("write %s: %w", p.Path, err)
	}
	return acp.WriteTextFileResponse{}, nil
}

// CreateTerminal implements acp.Client — spawns a command and streams its output into a buffer.
func (s *Session) CreateTerminal(
	_ context.Context,
	p acp.CreateTerminalRequest,
) (acp.CreateTerminalResponse, error) {
	cmd := exec.CommandContext(s.ctx, p.Command, p.Args...)
	if p.Cwd != nil {
		cmd.Dir = *p.Cwd
	}
	for _, e := range p.Env {
		cmd.Env = append(cmd.Env, e.Name+"="+e.Value)
	}

	t := &terminal{done: make(chan struct{})}
	r, w := io.Pipe()
	cmd.Stdout = w
	cmd.Stderr = w

	if err := cmd.Start(); err != nil {
		return acp.CreateTerminalResponse{}, fmt.Errorf("start terminal: %w", err)
	}

	t.cmd = cmd
	termID := fmt.Sprintf("term-%d", cmd.Process.Pid)

	go func() { _, _ = io.Copy(t, r) }()

	go func() {
		defer close(t.done)
		_ = cmd.Wait()
		_ = w.Close()
		code := 0
		if cmd.ProcessState != nil {
			code = cmd.ProcessState.ExitCode()
		}
		t.mu.Lock()
		t.exitCode = &code
		t.mu.Unlock()
	}()

	s.mu.Lock()
	s.terminals[termID] = t
	s.mu.Unlock()

	return acp.CreateTerminalResponse{TerminalId: termID}, nil
}

// TerminalOutput implements acp.Client — returns buffered output and clears the buffer.
func (s *Session) TerminalOutput(
	_ context.Context,
	p acp.TerminalOutputRequest,
) (acp.TerminalOutputResponse, error) {
	s.mu.Lock()
	t, ok := s.terminals[p.TerminalId]
	s.mu.Unlock()
	if !ok {
		return acp.TerminalOutputResponse{}, fmt.Errorf("unknown terminal %s", p.TerminalId)
	}

	t.mu.Lock()
	output := t.buf.String()
	t.buf.Reset()
	exitCode := t.exitCode
	t.mu.Unlock()

	resp := acp.TerminalOutputResponse{Output: output}
	if exitCode != nil {
		resp.ExitStatus = &acp.TerminalExitStatus{ExitCode: exitCode}
	}
	return resp, nil
}

// WaitForTerminalExit implements acp.Client — blocks until the command exits.
func (s *Session) WaitForTerminalExit(
	ctx context.Context,
	p acp.WaitForTerminalExitRequest,
) (acp.WaitForTerminalExitResponse, error) {
	s.mu.Lock()
	t, ok := s.terminals[p.TerminalId]
	s.mu.Unlock()
	if !ok {
		return acp.WaitForTerminalExitResponse{}, fmt.Errorf("unknown terminal %s", p.TerminalId)
	}

	select {
	case <-t.done:
		t.mu.Lock()
		code := t.exitCode
		t.mu.Unlock()
		return acp.WaitForTerminalExitResponse{ExitCode: code}, nil
	case <-ctx.Done():
		return acp.WaitForTerminalExitResponse{}, ctx.Err()
	}
}

// KillTerminalCommand implements acp.Client — sends SIGINT to the terminal process.
func (s *Session) KillTerminalCommand(
	_ context.Context,
	p acp.KillTerminalCommandRequest,
) (acp.KillTerminalCommandResponse, error) {
	s.mu.Lock()
	t, ok := s.terminals[p.TerminalId]
	s.mu.Unlock()
	if !ok {
		return acp.KillTerminalCommandResponse{}, nil
	}
	if t.cmd != nil && t.cmd.Process != nil {
		_ = t.cmd.Process.Signal(os.Interrupt)
		// Escalation to SIGKILL after 2s if still running
		go func() {
			select {
			case <-t.done:
				return
			case <-time.After(2 * time.Second):
				t.mu.Lock()
				if t.exitCode == nil && t.cmd.Process != nil {
					_ = t.cmd.Process.Kill()
				}
				t.mu.Unlock()
			}
		}()
	}
	return acp.KillTerminalCommandResponse{}, nil
}

// ReleaseTerminal implements acp.Client — kills the process and removes it from the map.
func (s *Session) ReleaseTerminal(
	_ context.Context,
	p acp.ReleaseTerminalRequest,
) (acp.ReleaseTerminalResponse, error) {
	s.mu.Lock()
	t, ok := s.terminals[p.TerminalId]
	if ok {
		delete(s.terminals, p.TerminalId)
	}
	s.mu.Unlock()

	if ok && t.cmd != nil && t.cmd.Process != nil {
		_ = t.cmd.Process.Kill()
	}
	return acp.ReleaseTerminalResponse{}, nil
}

func allowResponse(p acp.RequestPermissionRequest) acp.RequestPermissionResponse {
	for _, opt := range p.Options {
		if opt.Kind == acp.PermissionOptionKindAllowOnce ||
			opt.Kind == acp.PermissionOptionKindAllowAlways {
			return acp.RequestPermissionResponse{
				Outcome: acp.NewRequestPermissionOutcomeSelected(opt.OptionId),
			}
		}
	}
	return acp.RequestPermissionResponse{Outcome: acp.NewRequestPermissionOutcomeCancelled()}
}

func denyResponse(p acp.RequestPermissionRequest) acp.RequestPermissionResponse {
	for _, opt := range p.Options {
		if opt.Kind == acp.PermissionOptionKindRejectOnce ||
			opt.Kind == acp.PermissionOptionKindRejectAlways {
			return acp.RequestPermissionResponse{
				Outcome: acp.NewRequestPermissionOutcomeSelected(opt.OptionId),
			}
		}
	}
	return acp.RequestPermissionResponse{Outcome: acp.NewRequestPermissionOutcomeCancelled()}
}

// toolContentText extracts displayable text from ACP tool content blocks.
func toolContentText(content []acp.ToolCallContent) string {
	var sb strings.Builder
	for _, c := range content {
		if c.Content != nil && c.Content.Content.Text != nil {
			sb.WriteString(c.Content.Content.Text.Text)
		} else if c.Diff != nil {
			sb.WriteString(fmt.Sprintf("diff %s\n", c.Diff.Path))
		}
	}
	return sb.String()
}

func cwdFromStorage(stor storage.Session) string {
	cwd := ""
	if stor != nil {
		if m := stor.Meta(); m.CWD != "" {
			cwd = m.CWD
		}
	}
	if cwd == "" {
		cwd, _ = os.Getwd()
	}
	if abs, err := filepath.Abs(cwd); err == nil {
		return abs
	}
	return cwd
}
