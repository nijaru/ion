package acp

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// Backend implements the backend.Backend interface for external agent processes
// communicating via the Agent Connectivity Protocol (ACP).
type Backend struct {
	session *Session
	store   storage.Store
	sess    storage.Session
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
	if meta, ok := registry.GetMetadata(context.Background(), provider, model); ok {
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
			// New session
			status = fmt.Sprintf("Connected to %s", b.Provider())
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

func (b *Backend) SetStore(s storage.Store) {
	b.store = s
	b.session.store = s
}

func (b *Backend) SetSession(s storage.Session) {
	b.sess = s
	b.session.storage = s
}
