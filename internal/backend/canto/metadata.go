package canto

import (
	"context"
	"os"
	"slices"
	"strings"

	"github.com/nijaru/canto/prompt"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (b *Backend) Name() string {
	return "canto"
}

func (b *Backend) SetConfig(cfg *config.Config) {
	var copied *config.Config
	if cfg != nil {
		cfgCopy := *cfg
		copied = &cfgCopy
	}

	b.cfgMu.Lock()
	b.cfg = copied
	b.cfgMu.Unlock()

	b.updateRetryConfig(copied)
}

func (b *Backend) updateRetryConfig(cfg *config.Config) {
	b.mu.Lock()
	provider := b.compactLLM
	b.mu.Unlock()

	if retry, ok := retryProviderInChain(provider); ok {
		retry.Config.RetryForever = cfg.RetryUntilCancelledEnabled()
		retry.Config.RetryForeverTransportOnly = true
	}
}

func (b *Backend) configSnapshot() *config.Config {
	b.cfgMu.RLock()
	defer b.cfgMu.RUnlock()
	if b.cfg == nil {
		return nil
	}
	copied := *b.cfg
	return &copied
}

func providerFromConfig(cfg *config.Config) string {
	if cfg != nil && cfg.Provider != "" {
		return cfg.Provider
	}
	return os.Getenv("ION_PROVIDER")
}

func modelFromConfig(cfg *config.Config) string {
	if cfg != nil && cfg.Model != "" {
		return cfg.Model
	}
	m := os.Getenv("ION_MODEL")
	if i := strings.IndexByte(m, ' '); i > 0 {
		return strings.TrimSpace(m[i+1:])
	}
	return m
}

func contextLimitFromConfig(cfg *config.Config) int {
	if cfg != nil && cfg.ContextLimit > 0 {
		return cfg.ContextLimit
	}
	if limit, ok := registry.CachedContextLimitForConfig(cfg); ok {
		return limit
	}
	provider := providerFromConfig(cfg)
	model := modelFromConfig(cfg)
	if limit, ok := registry.CachedContextLimit(provider, model); ok {
		return limit
	}
	return 0
}

func (b *Backend) Provider() string {
	return providerFromConfig(b.configSnapshot())
}

func (b *Backend) Model() string {
	return modelFromConfig(b.configSnapshot())
}

func (b *Backend) ContextLimit() int {
	return contextLimitFromConfig(b.configSnapshot())
}

func (b *Backend) ToolSurface() backend.ToolSurface {
	b.mu.Lock()
	tools := b.tools
	b.mu.Unlock()
	if tools == nil {
		return backend.ToolSurface{}
	}
	names := tools.Names()
	cfg := b.configSnapshot()
	mode := "coding"
	if cfg != nil {
		mode = cfg.ActiveToolMode()
	}
	activeNames := activeToolNamesForMode(mode, names)
	threshold := prompt.DefaultLazyThreshold
	environment := ""
	if slices.Contains(names, "bash") {
		environment = b.executorEnvironmentPolicy().Summary()
	}
	return backend.ToolSurface{
		Count:         len(names),
		LazyThreshold: threshold,
		LazyEnabled:   len(names) > threshold,
		Names:         names,
		ActiveNames:   activeNames,
		Mode:          mode,
		Environment:   environment,
	}
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	status := "Ready"
	b.mu.Lock()
	sess := b.sess
	b.mu.Unlock()
	if sess != nil {
		if s, err := sess.LastStatus(context.Background()); err == nil && s != "" {
			status = s
		} else {
			status = "Connected via Canto"
		}
	}
	return backend.Bootstrap{
		Entries: []ionsession.Entry{},
		Status:  status,
	}
}

func (b *Backend) Session() ionsession.AgentSession {
	return &Session{backend: b}
}

func (b *Backend) SetStore(s storage.Store) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.ionStore = s
}

func (b *Backend) SetSession(s storage.Session) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.sess = s
}

func (s *Session) Events() <-chan ionsession.Event {
	return s.backend.events
}

func (s *Session) ID() string {
	return s.backend.id()
}

func (b *Backend) id() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.idLocked()
}

func (b *Backend) idLocked() string {
	if b.sess != nil {
		return b.sess.ID()
	}
	return ""
}

func (s *Session) Meta() map[string]string {
	return s.backend.meta()
}

func (b *Backend) meta() map[string]string {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.sess != nil {
		m := b.sess.Meta()
		return map[string]string{
			"model":  m.Model,
			"branch": m.Branch,
			"cwd":    m.CWD,
		}
	}
	return nil
}

func storageModelName(provider, model string) string {
	provider = strings.TrimSpace(provider)
	model = strings.TrimSpace(model)
	if provider == "" {
		return model
	}
	if model == "" {
		return provider
	}
	return provider + "/" + model
}
