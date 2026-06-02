package acp

import (
	"context"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Backend implements the backend.Backend interface for external agent processes
// communicating via the Agent Connectivity Protocol (ACP).
type Backend struct {
	session *Session
	store   session.SessionStore
	sess    session.SessionHandle
	cfg     *config.Config
}

func New() *Backend {
	return &Backend{
		session: newSession(),
	}
}

func (b *Backend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
}

func (b *Backend) Name() string {
	return "acp"
}

func (b *Backend) Provider() string {
	if b.cfg != nil && b.cfg.Provider != "" {
		return b.cfg.Provider
	}
	return "acp"
}

func (b *Backend) Model() string {
	if b.cfg != nil && b.cfg.Model != "" {
		return b.cfg.Model
	}
	return ""
}

func (b *Backend) ContextLimit() int {
	if b.cfg != nil && b.cfg.ContextLimit > 0 {
		return b.cfg.ContextLimit
	}
	provider := b.Provider()
	model := b.Model()
	if meta, ok := llm.GetMetadata(context.Background(), provider, model); ok {
		return meta.ContextLimit
	}
	return 0
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	status := "Ready"
	if b.sess != nil {
		if s, err := b.sess.LastStatus(context.Background()); err == nil && s != "" {
			status = s
		} else {
			status = "Connected via ACP"
		}
	}
	return backend.Bootstrap{
		Entries: []session.Entry{},
		Status:  status,
	}
}

func (b *Backend) Session() session.AgentSession {
	return b.session
}

func (b *Backend) SetStore(s session.SessionStore) {
	b.store = s
	b.session.store = s
}

func (b *Backend) SetSession(s session.SessionHandle) {
	b.sess = s
	b.session.storage = s
}
