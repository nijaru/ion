package backend

import (
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type Bootstrap struct {
	Entries []session.Entry
	Status  string
}

type Backend interface {
	Name() string
	Provider() string
	Model() string
	ContextLimit() int
	Bootstrap() Bootstrap
	Session() session.AgentSession
	SetStore(storage.Store)
	SetSession(storage.Session)
	SetConfig(*config.Config)
}
