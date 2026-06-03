package prompt

import (
	"context"
	"fmt"
	"math"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

const (
	defaultWarningThresholdPct  = 0.70
	defaultCriticalThresholdPct = 0.90
)

// BudgetLevel reports the current capacity state of a request.
type BudgetLevel string

const (
	BudgetOK       BudgetLevel = "ok"
	BudgetWarning  BudgetLevel = "warning"
	BudgetCritical BudgetLevel = "critical"
	BudgetExceeded BudgetLevel = "exceeded"
)

// BudgetStatus describes the current request's capacity against the configured
// token budget. PendingTokens is separated so callers can check prospective
// observations before they are appended to the model-visible request.
type BudgetStatus struct {
	Level                BudgetLevel
	MaxTokens            int
	CurrentTokens        int
	PendingTokens        int
	TotalTokens          int
	WarningThresholdPct  float64
	CriticalThresholdPct float64
	WarningThreshold     int
	CriticalThreshold    int
}

// NeedsCompaction reports whether the request has reached a point where a
// lighter-weight masking/compaction step should run before continuing.
func (s BudgetStatus) NeedsCompaction() bool {
	return s.Level == BudgetWarning || s.Level == BudgetCritical || s.Level == BudgetExceeded
}

// IsTerminal reports whether the request should stop instead of continuing.
func (s BudgetStatus) IsTerminal() bool {
	return s.Level == BudgetCritical || s.Level == BudgetExceeded
}

// UsageRatio returns the fraction of the max budget consumed.
func (s BudgetStatus) UsageRatio() float64 {
	if s.MaxTokens <= 0 {
		return 0
	}
	return float64(s.TotalTokens) / float64(s.MaxTokens)
}

// BudgetThresholdError reports that the request crossed the critical or hard
// token threshold before the provider call was made.
type BudgetThresholdError struct {
	Status BudgetStatus
}

func (e *BudgetThresholdError) Error() string {
	switch e.Status.Level {
	case BudgetExceeded:
		return fmt.Sprintf(
			"context budget exceeded: %d >= %d",
			e.Status.TotalTokens,
			e.Status.MaxTokens,
		)
	case BudgetCritical:
		return fmt.Sprintf(
			"context budget critical: %d >= %d (%d max)",
			e.Status.TotalTokens,
			e.Status.CriticalThreshold,
			e.Status.MaxTokens,
		)
	default:
		return fmt.Sprintf("context budget threshold reached: %s", e.Status.Level)
	}
}

// BudgetGuard checks request capacity before the provider call.
//
// WarningThresholdPct marks the point where callers should start masking or
// compacting aggressively. CriticalThresholdPct marks the point where the
// request should stop until compaction or masking reduces it.
type BudgetGuard struct {
	MaxTokens            int
	WarningThresholdPct  float64
	CriticalThresholdPct float64
	OnStatus             func(BudgetStatus)
}

// NewBudgetGuard creates a request-capacity guard with sensible defaults.
func NewBudgetGuard(maxTokens int) *BudgetGuard {
	return &BudgetGuard{
		MaxTokens:            maxTokens,
		WarningThresholdPct:  defaultWarningThresholdPct,
		CriticalThresholdPct: defaultCriticalThresholdPct,
	}
}

// Check evaluates current plus pending tokens against the configured budget.
func (g *BudgetGuard) Check(currentTokens, pendingTokens int) BudgetStatus {
	status := BudgetStatus{
		MaxTokens:            g.MaxTokens,
		CurrentTokens:        max(currentTokens, 0),
		PendingTokens:        max(pendingTokens, 0),
		WarningThresholdPct:  g.warningThresholdPct(),
		CriticalThresholdPct: g.criticalThresholdPct(),
	}
	status.TotalTokens = status.CurrentTokens + status.PendingTokens
	status.WarningThreshold = thresholdTokens(status.MaxTokens, status.WarningThresholdPct)
	status.CriticalThreshold = thresholdTokens(status.MaxTokens, status.CriticalThresholdPct)

	switch {
	case status.MaxTokens <= 0:
		status.Level = BudgetOK
	case status.TotalTokens >= status.MaxTokens:
		status.Level = BudgetExceeded
	case status.TotalTokens >= status.CriticalThreshold:
		status.Level = BudgetCritical
	case status.TotalTokens >= status.WarningThreshold:
		status.Level = BudgetWarning
	default:
		status.Level = BudgetOK
	}

	return status
}

// ApplyRequest estimates the built request and reports a threshold error once
// the request reaches the critical or hard limit.
func (g *BudgetGuard) ApplyRequest(
	ctx context.Context,
	p llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	status := g.Check(EstimateMessagesTokens(ctx, p, model, req.Messages), 0)
	if g.OnStatus != nil {
		g.OnStatus(status)
	}
	if status.IsTerminal() {
		return &BudgetThresholdError{Status: status}
	}
	return nil
}

func (g *BudgetGuard) warningThresholdPct() float64 {
	pct := g.WarningThresholdPct
	if pct <= 0 || pct > 1 {
		return defaultWarningThresholdPct
	}
	return pct
}

func (g *BudgetGuard) criticalThresholdPct() float64 {
	pct := g.CriticalThresholdPct
	if pct <= 0 || pct > 1 {
		return defaultCriticalThresholdPct
	}
	if pct < g.warningThresholdPct() {
		return g.warningThresholdPct()
	}
	return pct
}

func thresholdTokens(maxTokens int, thresholdPct float64) int {
	if maxTokens <= 0 {
		return 0
	}
	return int(math.Ceil(float64(maxTokens) * thresholdPct))
}
