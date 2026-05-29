package agent

import (
	"context"
	"strings"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/internal/models"
	"github.com/nijaru/ion/providers"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// Backend implements backend.Backend using the agent loop.
type Backend struct {
	cfg      *config.Config
	store    storage.Store
	 sess    storage.Session
	provider llm.Provider
	session  *SessionAdapter

	// toolExecutor is the tool execution function. Set via SetToolExecutor.
	toolExecutor ToolExecutor
	// tools are the available tool definitions. Set via SetTools.
	tools []AgentTool
}

var _ backend.Backend = (*Backend)(nil)

// NewBackend creates a new agent backend.
func NewBackend() *Backend {
	return &Backend{}
}

// SetToolExecutor sets the tool executor for the backend.
func (b *Backend) SetToolExecutor(exec ToolExecutor) {
	b.toolExecutor = exec
	// Reset session to pick up new executor
	b.session = nil
}

// SetTools sets the available tool definitions for the backend.
func (b *Backend) SetTools(tools []AgentTool) {
	b.tools = tools
	// Reset session to pick up new tools
	b.session = nil
}

// Name returns the backend name.
func (b *Backend) Name() string {
	return "agent"
}

// Provider returns the provider name.
func (b *Backend) Provider() string {
	if b.cfg == nil {
		return ""
	}
	return strings.TrimSpace(b.cfg.Provider)
}

// Model returns the model name.
func (b *Backend) Model() string {
	if b.cfg == nil {
		return ""
	}
	return strings.TrimSpace(b.cfg.Model)
}

// ContextLimit returns the context window size.
func (b *Backend) ContextLimit() int {
	if b.cfg == nil {
		return 0
	}
	if b.cfg.ContextLimit > 0 {
		return b.cfg.ContextLimit
	}

	// Try to get from model metadata
	if meta, ok := models.GetCachedMetadata(b.Provider(), b.Model()); ok {
		return meta.ContextLimit
	}

	return 0
}

// Bootstrap returns session bootstrap data.
func (b *Backend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: nil,
		Status:  "ready",
	}
}

// Session returns the agent session.
func (b *Backend) Session() session.AgentSession {
	if b.session == nil {
		b.session = b.createSession()
	}
	return b.session
}

// SetStore sets the storage store.
func (b *Backend) SetStore(store storage.Store) {
	b.store = store
	if b.session != nil {
		b.session.SetStore(store)
	}
}

// SetSession sets the storage session.
func (b *Backend) SetSession(sess storage.Session) {
	b.sess = sess
	if b.session != nil {
		b.session.SetSession(sess)
	}
}

// SetConfig sets the configuration.
func (b *Backend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
	// Reset session to pick up new config
	b.session = nil
}

// createSession creates a new session adapter from the current config.
func (b *Backend) createSession() *SessionAdapter {
	if b.cfg == nil {
		return NewSessionAdapter(&SessionAdapterConfig{
			ID: "default",
		})
	}

	// Create provider from config
	provider, err := providers.NewProviderFromConfig(b.cfg)
	if err != nil {
		// Return a basic session without streaming
		return NewSessionAdapter(&SessionAdapterConfig{
			ID: b.sessionID(),
		})
	}
	b.provider = provider

	// Get model from config
	model := llm.Model{ID: b.Model()}
	if meta, ok := models.GetCachedMetadata(b.Provider(), b.Model()); ok {
		model.ContextWindow = meta.ContextLimit
		model.CostPer1MIn = meta.InputPrice
		model.CostPer1MOut = meta.OutputPrice
	}

	// Create stream function
	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return provider.Stream(ctx, req)
	}

	// Create session adapter
	sessionCfg := &SessionAdapterConfig{
		ID:           b.sessionID(),
		Model:        model,
		StreamFn:     streamFn,
		ToolExecutor: b.toolExecutor,
		Tools:        b.tools,
	}

	adapter := NewSessionAdapter(sessionCfg)

	// Set store and session if available
	if b.store != nil {
		adapter.SetStore(b.store)
	}
	if b.sess != nil {
		adapter.SetSession(b.sess)
	}

	return adapter
}

// sessionID returns the session ID from config or default.
func (b *Backend) sessionID() string {
	return "default"
}
