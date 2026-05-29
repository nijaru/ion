package prompt

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/storage/session"
)

const (
	defaultMaskMaxContentTokens = 256
	defaultMaskMinKeepMessages  = 3
)

// ObservationMasker replaces older oversized tool outputs with compact
// placeholders before the request is sent to the provider. The original
// event log remains unchanged, so the placeholders stay reversible via the
// originating event IDs in session history.
type ObservationMasker struct {
	BudgetGuard      *BudgetGuard
	MaxContentTokens int
	MinKeepMessages  int
	Placeholder      func(entry session.HistoryEntry, contentTokens int) string
}

// NewObservationMasker creates a masker with default thresholds.
func NewObservationMasker(guard *BudgetGuard) *ObservationMasker {
	return &ObservationMasker{
		BudgetGuard:      guard,
		MaxContentTokens: defaultMaskMaxContentTokens,
		MinKeepMessages:  defaultMaskMinKeepMessages,
	}
}

// History returns a request processor that appends the masked model-visible
// history for the session.
func (m *ObservationMasker) History() RequestProcessor {
	return RequestProcessorFunc(func(
		ctx context.Context,
		p llm.Provider,
		model string,
		sess *session.Session,
		req *llm.Request,
	) error {
		entries, err := sess.EffectiveEntries()
		if err != nil {
			return err
		}
		masked, _ := m.MaskEntries(ctx, p, model, req.Messages, entries)
		req.CachePrefixMessages = len(req.Messages) + countPrefixContextMessages(masked)
		for _, entry := range masked {
			req.AppendMessage(entry.Message)
		}
		return nil
	})
}

// MaskEntries returns a masked copy of entries plus the final budget status
// after masking. Prefix messages are included in the budget estimate but are
// never rewritten.
func (m *ObservationMasker) MaskEntries(
	ctx context.Context,
	p llm.Provider,
	model string,
	prefix []llm.Message,
	entries []session.HistoryEntry,
) ([]session.HistoryEntry, BudgetStatus) {
	masked := cloneHistoryEntries(entries)
	status := m.statusForEntries(ctx, p, model, prefix, masked)
	if !m.shouldMask(status) {
		return masked, status
	}

	cutoff := len(masked) - m.minKeepMessages()
	if cutoff < 0 {
		cutoff = 0
	}

	for i := 0; i < cutoff; i++ {
		entry := masked[i]
		if !m.shouldMaskEntry(entry) {
			continue
		}
		contentTokens := EstimateTokens(entry.Message.Content)
		entry.Message.Content = m.placeholder(entry, contentTokens)
		masked[i] = entry

		status = m.statusForEntries(ctx, p, model, prefix, masked)
		// Do not break early even if budget is met.
		// Masking all eligible older messages creates a predictable stable boundary
		// that prevents the prompt cache from busting on every subsequent turn.
	}

	return masked, status
}

func (m *ObservationMasker) shouldMaskEntry(entry session.HistoryEntry) bool {
	if entry.Message.Role != llm.RoleTool {
		return false
	}
	return EstimateTokens(entry.Message.Content) >= m.maxContentTokens()
}

func (m *ObservationMasker) placeholder(entry session.HistoryEntry, contentTokens int) string {
	if m.Placeholder != nil {
		return m.Placeholder(entry, contentTokens)
	}
	if entry.Message.ToolID != "" {
		return fmt.Sprintf(
			"[Observation masked: event=%s tool_id=%s tokens=%d]",
			entry.EventID,
			entry.Message.ToolID,
			contentTokens,
		)
	}
	return fmt.Sprintf(
		"[Observation masked: event=%s tokens=%d]",
		entry.EventID,
		contentTokens,
	)
}

func (m *ObservationMasker) shouldMask(status BudgetStatus) bool {
	if m.BudgetGuard == nil {
		return true
	}
	return status.NeedsCompaction()
}

func (m *ObservationMasker) statusForEntries(
	ctx context.Context,
	p llm.Provider,
	model string,
	prefix []llm.Message,
	entries []session.HistoryEntry,
) BudgetStatus {
	if m.BudgetGuard == nil {
		return BudgetStatus{}
	}
	messages := make([]llm.Message, 0, len(prefix)+len(entries))
	messages = append(messages, prefix...)
	for _, entry := range entries {
		messages = append(messages, entry.Message)
	}
	return m.BudgetGuard.Check(EstimateMessagesTokens(ctx, p, model, messages), 0)
}

func (m *ObservationMasker) maxContentTokens() int {
	if m.MaxContentTokens <= 0 {
		return defaultMaskMaxContentTokens
	}
	return m.MaxContentTokens
}

func (m *ObservationMasker) minKeepMessages() int {
	if m.MinKeepMessages < 0 {
		return 0
	}
	if m.MinKeepMessages == 0 {
		return defaultMaskMinKeepMessages
	}
	return m.MinKeepMessages
}

func cloneHistoryEntries(entries []session.HistoryEntry) []session.HistoryEntry {
	cloned := make([]session.HistoryEntry, len(entries))
	copy(cloned, entries)
	return cloned
}
