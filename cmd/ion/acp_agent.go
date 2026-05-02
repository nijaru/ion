package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"path/filepath"
	"strings"
	"sync"

	acp "github.com/coder/acp-go-sdk"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/privacy"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/nijaru/ion/internal/tooldisplay"
)

type acpRuntimeFactory interface {
	Open(
		ctx context.Context,
		cwd string,
		sessionID string,
	) (ionsession.AgentSession, func() error, error)
}

type ionACPRuntimeFactory struct {
	store              storage.Store
	cfg                *config.Config
	branch             string
	mode               ionsession.Mode
	acpCommandOverride string
}

func (f ionACPRuntimeFactory) Open(
	ctx context.Context,
	cwd string,
	sessionID string,
) (ionsession.AgentSession, func() error, error) {
	runtimeCfg := *f.cfg
	if sessionID != "" {
		if err := applySessionConfigFromMetadata(ctx, f.store, sessionID, &runtimeCfg); err != nil {
			return nil, nil, err
		}
	}
	resolved, _, err := startupRuntimeConfig(ctx, &runtimeCfg, sessionID, false)
	if err != nil {
		return nil, nil, err
	}

	b, sess, err := openRuntime(
		ctx,
		f.store,
		cwd,
		f.branch,
		resolved,
		f.acpCommandOverride,
		sessionID,
	)
	if err != nil {
		return nil, nil, err
	}
	agent := b.Session()
	if agent == nil {
		_ = closeRuntimeHandles(nil, sess, nil)
		return nil, nil, fmt.Errorf("runtime has no agent session")
	}
	configureSessionMode(agent, f.mode)
	return agent, func() error {
		return closeRuntimeHandles(agent, sess, nil)
	}, nil
}

type ionACPAgent struct {
	conn    *acp.AgentSideConnection
	factory acpRuntimeFactory
	version string
	mode    ionsession.Mode

	mu       sync.Mutex
	sessions map[string]*ionACPHeadlessSession
}

type ionACPHeadlessSession struct {
	agent ionsession.AgentSession
	close func() error
	cwd   string
	mode  ionsession.Mode
}

var (
	_ acp.Agent       = (*ionACPAgent)(nil)
	_ acp.AgentLoader = (*ionACPAgent)(nil)
)

func newIonACPAgent(factory acpRuntimeFactory, version string, mode ionsession.Mode) *ionACPAgent {
	return &ionACPAgent{
		factory:  factory,
		version:  version,
		mode:     mode,
		sessions: make(map[string]*ionACPHeadlessSession),
	}
}

func runACPAgent(
	ctx context.Context,
	r io.Reader,
	w io.Writer,
	store storage.Store,
	cfg *config.Config,
	branch string,
	mode ionsession.Mode,
	acpCommandOverride string,
) error {
	agent := newIonACPAgent(ionACPRuntimeFactory{
		store:              store,
		cfg:                cfg,
		branch:             branch,
		mode:               mode,
		acpCommandOverride: acpCommandOverride,
	}, version, mode)
	conn := acp.NewAgentSideConnection(agent, w, r)
	agent.SetAgentConnection(conn)

	select {
	case <-conn.Done():
		return agent.Close()
	case <-ctx.Done():
		return errors.Join(agent.Close(), ctx.Err())
	}
}

func (a *ionACPAgent) SetAgentConnection(conn *acp.AgentSideConnection) {
	a.conn = conn
}

func (a *ionACPAgent) Close() error {
	a.mu.Lock()
	sessions := make([]*ionACPHeadlessSession, 0, len(a.sessions))
	for id, sess := range a.sessions {
		sessions = append(sessions, sess)
		delete(a.sessions, id)
	}
	a.mu.Unlock()

	var errs []error
	for _, sess := range sessions {
		if sess.close != nil {
			errs = append(errs, sess.close())
		}
	}
	return errors.Join(errs...)
}

func (a *ionACPAgent) Initialize(
	_ context.Context,
	_ acp.InitializeRequest,
) (acp.InitializeResponse, error) {
	return acp.InitializeResponse{
		ProtocolVersion: acp.ProtocolVersionNumber,
		AgentInfo: &acp.Implementation{
			Name:    "ion",
			Version: a.version,
		},
		AgentCapabilities: acp.AgentCapabilities{
			LoadSession: true,
			PromptCapabilities: acp.PromptCapabilities{
				EmbeddedContext: false,
			},
		},
		AuthMethods: []acp.AuthMethod{},
	}, nil
}

func (a *ionACPAgent) Authenticate(
	context.Context,
	acp.AuthenticateRequest,
) (acp.AuthenticateResponse, error) {
	return acp.AuthenticateResponse{}, nil
}

func (a *ionACPAgent) NewSession(
	ctx context.Context,
	params acp.NewSessionRequest,
) (acp.NewSessionResponse, error) {
	sid, sess, err := a.openSession(ctx, params.Cwd, "")
	if err != nil {
		return acp.NewSessionResponse{}, err
	}
	return acp.NewSessionResponse{
		SessionId: acp.SessionId(sid),
		Modes:     acpModeState(sess.mode),
	}, nil
}

func (a *ionACPAgent) LoadSession(
	ctx context.Context,
	params acp.LoadSessionRequest,
) (acp.LoadSessionResponse, error) {
	_, sess, err := a.openSession(ctx, params.Cwd, string(params.SessionId))
	if err != nil {
		return acp.LoadSessionResponse{}, err
	}
	return acp.LoadSessionResponse{Modes: acpModeState(sess.mode)}, nil
}

func (a *ionACPAgent) openSession(
	ctx context.Context,
	cwd string,
	sessionID string,
) (string, *ionACPHeadlessSession, error) {
	if cwd == "" {
		return "", nil, fmt.Errorf("cwd is required")
	}
	absCWD, err := filepath.Abs(cwd)
	if err != nil {
		return "", nil, fmt.Errorf("resolve cwd: %w", err)
	}

	agent, closeFn, err := a.factory.Open(ctx, absCWD, sessionID)
	if err != nil {
		return "", nil, err
	}
	sid := strings.TrimSpace(sessionID)
	if sid == "" {
		sid = strings.TrimSpace(agent.ID())
	}
	if sid == "" {
		_ = closeFn()
		return "", nil, fmt.Errorf("runtime returned empty session ID")
	}

	sess := &ionACPHeadlessSession{
		agent: agent,
		close: closeFn,
		cwd:   absCWD,
		mode:  a.mode,
	}
	a.mu.Lock()
	if old := a.sessions[sid]; old != nil && old.close != nil {
		_ = old.close()
	}
	a.sessions[sid] = sess
	a.mu.Unlock()
	return sid, sess, nil
}

func (a *ionACPAgent) Prompt(
	ctx context.Context,
	params acp.PromptRequest,
) (acp.PromptResponse, error) {
	sid := string(params.SessionId)
	sess, err := a.session(sid)
	if err != nil {
		return acp.PromptResponse{}, err
	}
	prompt, err := acpPromptText(params.Prompt)
	if err != nil {
		return acp.PromptResponse{}, err
	}
	if err := sess.agent.SubmitTurn(ctx, prompt); err != nil {
		return acp.PromptResponse{}, err
	}

	for {
		select {
		case event, ok := <-sess.agent.Events():
			if !ok {
				return acp.PromptResponse{}, fmt.Errorf(
					"session %s closed before turn finished",
					sid,
				)
			}
			done, stop, err := a.forwardEvent(ctx, sid, sess, event)
			if err != nil {
				return acp.PromptResponse{}, err
			}
			if done {
				return acp.PromptResponse{StopReason: stop}, nil
			}
		case <-ctx.Done():
			_ = sess.agent.CancelTurn(context.Background())
			return acp.PromptResponse{StopReason: acp.StopReasonCancelled}, nil
		}
	}
}

func (a *ionACPAgent) Cancel(ctx context.Context, params acp.CancelNotification) error {
	sess, err := a.session(string(params.SessionId))
	if err != nil {
		return nil
	}
	return sess.agent.CancelTurn(ctx)
}

func (a *ionACPAgent) SetSessionMode(
	ctx context.Context,
	params acp.SetSessionModeRequest,
) (acp.SetSessionModeResponse, error) {
	sess, err := a.session(string(params.SessionId))
	if err != nil {
		return acp.SetSessionModeResponse{}, err
	}
	mode, err := modeFromACPID(params.ModeId)
	if err != nil {
		return acp.SetSessionModeResponse{}, err
	}
	sess.mode = mode
	configureSessionMode(sess.agent, mode)
	if a.conn != nil {
		err = a.conn.SessionUpdate(ctx, acp.SessionNotification{
			SessionId: params.SessionId,
			Update: acp.SessionUpdate{
				CurrentModeUpdate: &acp.SessionCurrentModeUpdate{
					CurrentModeId: acpModeID(mode),
				},
			},
		})
	}
	return acp.SetSessionModeResponse{}, err
}

func (a *ionACPAgent) session(sessionID string) (*ionACPHeadlessSession, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	sess := a.sessions[sessionID]
	if sess == nil {
		return nil, fmt.Errorf("session %s not found", sessionID)
	}
	return sess, nil
}

func (a *ionACPAgent) forwardEvent(
	ctx context.Context,
	sessionID string,
	sess *ionACPHeadlessSession,
	event ionsession.Event,
) (bool, acp.StopReason, error) {
	switch e := event.(type) {
	case ionsession.TurnStarted:
		return false, "", nil
	case ionsession.TurnFinished:
		return true, acp.StopReasonEndTurn, nil
	case ionsession.AgentDelta:
		return false, "", a.sessionUpdate(ctx, sessionID, acp.UpdateAgentMessageText(e.Delta))
	case ionsession.ThinkingDelta:
		return false, "", a.sessionUpdate(ctx, sessionID, acp.UpdateAgentThoughtText(e.Delta))
	case ionsession.AgentMessage:
		if e.Reasoning != "" {
			if err := a.sessionUpdate(ctx, sessionID, acp.UpdateAgentThoughtText(e.Reasoning)); err != nil {
				return false, "", err
			}
		}
		return false, "", a.sessionUpdate(ctx, sessionID, acp.UpdateAgentMessageText(e.Message))
	case ionsession.ToolCallStarted:
		return false, "", a.sessionUpdate(ctx, sessionID, acpToolCallStart(sess.cwd, e))
	case ionsession.ToolOutputDelta:
		return false, "", a.sessionUpdate(ctx, sessionID, acpToolOutputDelta(e))
	case ionsession.ToolResult:
		return false, "", a.sessionUpdate(ctx, sessionID, acpToolCallResult(e))
	case ionsession.ApprovalRequest:
		if err := a.requestPermission(ctx, sessionID, sess, e); err != nil {
			return false, "", err
		}
		return false, "", nil
	case ionsession.Error:
		if e.Err != nil {
			return false, "", e.Err
		}
		return false, "", fmt.Errorf("session error")
	default:
		return false, "", nil
	}
}

func (a *ionACPAgent) sessionUpdate(
	ctx context.Context,
	sessionID string,
	update acp.SessionUpdate,
) error {
	if a.conn == nil {
		return nil
	}
	return a.conn.SessionUpdate(ctx, acp.SessionNotification{
		SessionId: acp.SessionId(sessionID),
		Update:    update,
	})
}

func (a *ionACPAgent) requestPermission(
	ctx context.Context,
	sessionID string,
	sess *ionACPHeadlessSession,
	req ionsession.ApprovalRequest,
) error {
	if a.conn == nil {
		return sess.agent.Approve(ctx, req.RequestID, false)
	}
	title := privacy.Redact(tooldisplay.Title(req.ToolName, req.Args, tooldisplay.Options{
		Workdir: sess.cwd,
		Width:   100,
	}))
	redactedArgs := privacy.Redact(req.Args)
	kind := acpToolKind(req.ToolName)
	status := acp.ToolCallStatusPending
	resp, err := a.conn.RequestPermission(ctx, acp.RequestPermissionRequest{
		SessionId: acp.SessionId(sessionID),
		ToolCall: acp.RequestPermissionToolCall{
			ToolCallId: acp.ToolCallId(req.RequestID),
			Title:      &title,
			Kind:       &kind,
			Status:     &status,
			RawInput:   acpRawInput(redactedArgs),
			Locations:  acpLocations(req.Args),
		},
		Options: []acp.PermissionOption{
			{
				Kind:     acp.PermissionOptionKindAllowOnce,
				Name:     "Allow once",
				OptionId: acp.PermissionOptionId("allow"),
			},
			{
				Kind:     acp.PermissionOptionKindRejectOnce,
				Name:     "Reject",
				OptionId: acp.PermissionOptionId("reject"),
			},
		},
	})
	if err != nil {
		return err
	}
	approved := resp.Outcome.Selected != nil &&
		string(resp.Outcome.Selected.OptionId) == "allow"
	return sess.agent.Approve(ctx, req.RequestID, approved)
}

func acpPromptText(blocks []acp.ContentBlock) (string, error) {
	var b strings.Builder
	for _, block := range blocks {
		switch {
		case block.Text != nil:
			b.WriteString(block.Text.Text)
		case block.ResourceLink != nil:
			if b.Len() > 0 {
				b.WriteString("\n\n")
			}
			b.WriteString(block.ResourceLink.Name)
			b.WriteString(": ")
			b.WriteString(block.ResourceLink.Uri)
		default:
			return "", fmt.Errorf("unsupported ACP prompt content block")
		}
	}
	return strings.TrimSpace(b.String()), nil
}

func acpToolCallStart(workdir string, e ionsession.ToolCallStarted) acp.SessionUpdate {
	title := privacy.Redact(tooldisplay.Title(e.ToolName, e.Args, tooldisplay.Options{
		Workdir: workdir,
		Width:   100,
	}))
	redactedArgs := privacy.Redact(e.Args)
	return acp.StartToolCall(
		acp.ToolCallId(e.ToolUseID),
		title,
		acp.WithStartKind(acpToolKind(e.ToolName)),
		acp.WithStartStatus(acp.ToolCallStatusPending),
		acp.WithStartRawInput(acpRawInput(redactedArgs)),
		acp.WithStartLocations(acpLocations(e.Args)),
	)
}

func acpToolOutputDelta(e ionsession.ToolOutputDelta) acp.SessionUpdate {
	delta := privacy.Redact(e.Delta)
	return acp.UpdateToolCall(
		acp.ToolCallId(e.ToolUseID),
		acp.WithUpdateStatus(acp.ToolCallStatusInProgress),
		acp.WithUpdateContent([]acp.ToolCallContent{
			acp.ToolContent(acp.TextBlock(delta)),
		}),
	)
}

func acpToolCallResult(e ionsession.ToolResult) acp.SessionUpdate {
	status := acp.ToolCallStatusCompleted
	output := e.Result
	if e.Error != nil {
		status = acp.ToolCallStatusFailed
		if output == "" {
			output = e.Error.Error()
		}
	}
	output = privacy.Redact(output)
	return acp.UpdateToolCall(
		acp.ToolCallId(e.ToolUseID),
		acp.WithUpdateStatus(status),
		acp.WithUpdateRawOutput(output),
		acp.WithUpdateContent([]acp.ToolCallContent{
			acp.ToolContent(acp.TextBlock(output)),
		}),
	)
}

func acpToolKind(name string) acp.ToolKind {
	switch strings.ToLower(strings.TrimSpace(name)) {
	case "read":
		return acp.ToolKindRead
	case "write", "edit", "multi_edit":
		return acp.ToolKindEdit
	case "list", "grep", "glob":
		return acp.ToolKindSearch
	case "bash":
		return acp.ToolKindExecute
	default:
		return acp.ToolKindOther
	}
}

func acpRawInput(args string) any {
	args = strings.TrimSpace(args)
	if args == "" {
		return nil
	}
	var value any
	if err := json.Unmarshal([]byte(args), &value); err == nil {
		return value
	}
	return args
}

func acpLocations(args string) []acp.ToolCallLocation {
	var raw map[string]any
	if err := json.Unmarshal([]byte(args), &raw); err != nil {
		return nil
	}
	for _, key := range []string{"file_path", "path"} {
		value, ok := raw[key].(string)
		if ok && strings.TrimSpace(value) != "" {
			return []acp.ToolCallLocation{{Path: value}}
		}
	}
	return nil
}

func acpModeState(mode ionsession.Mode) *acp.SessionModeState {
	return &acp.SessionModeState{
		CurrentModeId: acpModeID(mode),
		AvailableModes: []acp.SessionMode{
			{Id: acp.SessionModeId("read"), Name: "READ"},
			{Id: acp.SessionModeId("edit"), Name: "EDIT"},
			{Id: acp.SessionModeId("auto"), Name: "AUTO"},
		},
	}
}

func acpModeID(mode ionsession.Mode) acp.SessionModeId {
	switch mode {
	case ionsession.ModeRead:
		return acp.SessionModeId("read")
	case ionsession.ModeYolo:
		return acp.SessionModeId("auto")
	default:
		return acp.SessionModeId("edit")
	}
}

func modeFromACPID(id acp.SessionModeId) (ionsession.Mode, error) {
	switch strings.ToLower(strings.TrimSpace(string(id))) {
	case "read":
		return ionsession.ModeRead, nil
	case "edit", "":
		return ionsession.ModeEdit, nil
	case "auto", "yolo":
		return ionsession.ModeYolo, nil
	default:
		return ionsession.ModeEdit, fmt.Errorf("unsupported ACP session mode %q", id)
	}
}
