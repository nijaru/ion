package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"path/filepath"
	"strings"
	"sync"

	acp "github.com/coder/acp-go-sdk"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
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
		true,
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
