package canto

import (
	"sync"

	cantofw "github.com/nijaru/canto"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

const proactiveCompactThreshold = 0.60

type Backend struct {
	harness *cantofw.Harness
	store   session.Store
	events  chan ionsession.Event
	cfg     *config.Config
	cfgMu   sync.RWMutex
	llm     llm.Provider

	ionStore   storage.Store
	sess       storage.Session
	tools      *tool.Registry
	compactLLM llm.Provider

	mu        sync.Mutex
	turn      turnState
	closeOnce sync.Once
	wg        sync.WaitGroup
}

func New() *Backend {
	return &Backend{
		events: make(chan ionsession.Event, 100),
		turn:   newTurnState(),
	}
}

// Session is Ion's native AgentSession adapter over Canto's harness session.
// Backend owns product/runtime configuration; Session owns turn control.
type Session struct {
	backend *Backend
}

var (
	_ backend.Backend            = (*Backend)(nil)
	_ ionsession.AgentSession    = (*Session)(nil)
	_ ionsession.SteeringSession = (*Session)(nil)
)
