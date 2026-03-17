package acp

import (
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// Backend implements the backend.Backend interface for external agent processes
// communicating via the Agent Connectivity Protocol (ACP).
type Backend struct {
	session *Session
	store   storage.Store
	sess    storage.Session
}

func New() *Backend {
	return &Backend{
		session: newSession(),
	}
}

func (b *Backend) Name() string {
	return "acp"
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []session.Entry{
			{Role: session.System, Content: "External Agent Session (ACP)"},
		},
		Status: "Ready to connect to external agent...",
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
