package canto

import (
	"context"
	"fmt"
	"path/filepath"
	"strings"

	"github.com/nijaru/canto/governor"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/prompt"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/config"
)

const ionCompactionGuidance = `Summarize this Ion coding session for reliable continuation.

Preserve:
- current user goal and immediate next step
- files, packages, task IDs, commands, and commits that matter
- decisions, constraints, and unresolved blockers
- tool failures, root causes, and verification status

Discard:
- transient command noise, repeated stack traces, and already-resolved detours
- generic conversation filler

Write concise structured notes. Prefer exact paths, symbols, and IDs over prose.`

func compactionMessage(extra string) string {
	extra = strings.TrimSpace(extra)
	if extra == "" {
		return ionCompactionGuidance
	}
	return ionCompactionGuidance + "\n\nUser guidance:\n" + extra
}

type compactionRuntime struct {
	store     session.Store
	provider  llm.Provider
	sessionID string
	model     string
	maxTokens int
}

func (b *Backend) compactionRuntimeSnapshot() compactionRuntime {
	cfg := b.configSnapshot()
	runtime := compactionRuntime{
		model:     modelFromConfig(cfg),
		maxTokens: contextLimitFromConfig(cfg),
	}

	b.mu.Lock()
	runtime.store = b.store
	runtime.provider = b.compactLLM
	runtime.sessionID = b.idLocked()
	b.mu.Unlock()

	return runtime
}

func (b *Backend) shouldProactivelyCompact(ctx context.Context) (bool, error) {
	return b.compactionRuntimeSnapshot().shouldProactivelyCompact(ctx)
}

func (r compactionRuntime) shouldProactivelyCompact(ctx context.Context) (bool, error) {
	if r.store == nil || r.provider == nil || r.sessionID == "" ||
		r.model == "" || r.maxTokens <= 0 {
		return false, nil
	}

	sess, err := r.store.Load(ctx, r.sessionID)
	if err != nil {
		return false, err
	}
	messages, err := sess.EffectiveMessages()
	if err != nil {
		return false, err
	}

	threshold := int(float64(r.maxTokens) * proactiveCompactThreshold)
	if threshold <= 0 {
		threshold = r.maxTokens
	}
	used := prompt.EstimateMessagesTokens(ctx, r.provider, r.model, messages)
	return used >= threshold && used < r.maxTokens, nil
}

func (b *Backend) Compact(ctx context.Context) (bool, error) {
	return b.compactionRuntimeSnapshot().compact(ctx)
}

func (r compactionRuntime) compact(ctx context.Context) (bool, error) {
	if r.store == nil {
		return false, fmt.Errorf("backend store not initialized")
	}
	if r.sessionID == "" {
		return false, fmt.Errorf("session not initialized")
	}

	sess, err := r.store.Load(ctx, r.sessionID)
	if err != nil {
		return false, err
	}
	return r.compactSession(ctx, sess)
}

func (b *Backend) compactSession(ctx context.Context, sess *session.Session) (bool, error) {
	return b.compactionRuntimeSnapshot().compactSession(ctx, sess)
}

func (r compactionRuntime) compactSession(ctx context.Context, sess *session.Session) (bool, error) {
	if r.provider == nil {
		return false, fmt.Errorf("backend compaction provider not initialized")
	}
	if r.model == "" {
		return false, fmt.Errorf("model not configured")
	}
	if r.maxTokens <= 0 {
		return false, fmt.Errorf("context limit unavailable for model %s", r.model)
	}

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		return false, err
	}

	result, err := governor.CompactSession(ctx, r.provider, r.model, sess, governor.CompactOptions{
		MaxTokens:  r.maxTokens,
		OffloadDir: filepath.Join(dataDir, "artifacts"),
		Message:    compactionMessage(""),
	})
	if err != nil {
		return false, err
	}
	return result.Compacted, nil
}
