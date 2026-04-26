package backend

import (
	"context"

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

type Compactor interface {
	Compact(context.Context) (bool, error)
}

type PolicyConfigurer interface {
	SetPolicyConfig(*PolicyConfig)
}

type ToolSurface struct {
	Count         int
	LazyThreshold int
	LazyEnabled   bool
	Names         []string
}

type ToolSummarizer interface {
	ToolSurface() ToolSurface
}
