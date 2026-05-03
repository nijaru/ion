package canto

import (
	"context"
	"sync"

	cantofw "github.com/nijaru/canto"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto/tools"
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
	llm     llm.Provider

	ionStore   storage.Store
	sess       storage.Session
	tools      *tool.Registry
	compactLLM llm.Provider
	steering   *steeringMutator

	mu            sync.Mutex
	cancel        context.CancelFunc
	turnSeq       uint64
	turnActive    bool
	activeToolIDs map[string]struct{}
	closeOnce     sync.Once
	wg            sync.WaitGroup

	policy   *backend.PolicyEngine
	approver *tools.ApprovalManager
}

func New() *Backend {
	return &Backend{
		events:        make(chan ionsession.Event, 100),
		policy:        backend.NewPolicyEngine(),
		approver:      tools.NewApprovalManager(),
		activeToolIDs: make(map[string]struct{}),
	}
}
