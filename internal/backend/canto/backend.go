package canto

import (
	"context"
	"sync"

	"github.com/nijaru/canto/agent"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/memory"
	"github.com/nijaru/canto/runtime"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/canto/tool/mcp"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto/tools"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

const proactiveCompactThreshold = 0.60

type Backend struct {
	runner *runtime.Runner
	store  session.Store
	agent  *agent.BaseAgent
	events chan ionsession.Event
	cfg    *config.Config
	llm    llm.Provider

	ionStore   storage.Store
	sess       storage.Session
	memory     *memory.Manager
	coreMemory *memory.CoreStore
	tools      *tool.Registry
	compactLLM llm.Provider

	mu         sync.Mutex
	cancel     context.CancelFunc
	stopWatch  context.CancelFunc
	turnSeq    uint64
	turnActive bool
	closeOnce  sync.Once
	wg         sync.WaitGroup

	policy     *backend.PolicyEngine
	approver   *tools.ApprovalManager
	mcpClients []*mcp.Client
}

func New() *Backend {
	return &Backend{
		events:     make(chan ionsession.Event, 100),
		policy:     backend.NewPolicyEngine(),
		approver:   tools.NewApprovalManager(),
		mcpClients: make([]*mcp.Client, 0),
	}
}
