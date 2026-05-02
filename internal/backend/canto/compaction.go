package canto

import (
	"context"
	"fmt"
	"path/filepath"
	"strings"

	"github.com/nijaru/canto/governor"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/config"
)

const ionCompactionGuidance = `Summarize this Ion coding session for reliable continuation.

Preserve:
- current user goal and immediate next step
- files, packages, task IDs, commands, and commits that matter
- decisions, constraints, approvals, denials, and unresolved blockers
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

func (b *Backend) shouldProactivelyCompact(ctx context.Context) (bool, error) {
	b.mu.Lock()
	sess := b.sess
	store := b.store
	provider := b.compactLLM
	sessionID := b.ID()
	model := b.Model()
	limit := b.ContextLimit()
	b.mu.Unlock()

	if sess == nil || store == nil || provider == nil || sessionID == "" || model == "" ||
		limit <= 0 {
		return false, nil
	}

	inputTokens, outputTokens, _, err := sess.Usage(ctx)
	if err != nil {
		return false, err
	}

	threshold := int(float64(limit) * proactiveCompactThreshold)
	if threshold <= 0 {
		threshold = limit
	}
	used := inputTokens + outputTokens
	return used >= threshold && used < limit, nil
}

func (b *Backend) Compact(ctx context.Context) (bool, error) {
	b.mu.Lock()
	sessionID := b.ID()
	store := b.store
	b.mu.Unlock()

	if store == nil {
		return false, fmt.Errorf("backend store not initialized")
	}
	if sessionID == "" {
		return false, fmt.Errorf("session not initialized")
	}

	sess, err := store.Load(ctx, sessionID)
	if err != nil {
		return false, err
	}
	return b.compactSession(ctx, sess)
}

func (b *Backend) compactSession(ctx context.Context, sess *session.Session) (bool, error) {
	b.mu.Lock()
	provider := b.compactLLM
	model := b.Model()
	maxTokens := b.ContextLimit()
	b.mu.Unlock()

	if provider == nil {
		return false, fmt.Errorf("backend compaction provider not initialized")
	}
	if model == "" {
		return false, fmt.Errorf("model not configured")
	}
	if maxTokens <= 0 {
		return false, fmt.Errorf("context limit unavailable for model %s", model)
	}

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		return false, err
	}

	result, err := governor.CompactSession(ctx, provider, model, sess, governor.CompactOptions{
		MaxTokens:  maxTokens,
		OffloadDir: filepath.Join(dataDir, "artifacts"),
		Message:    compactionMessage(""),
	})
	if err != nil {
		return false, err
	}
	return result.Compacted, nil
}
