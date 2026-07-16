package app

import (
	"testing"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

func TestDecideSubmitPreflightEnforcesSessionCostLimit(t *testing.T) {
	tests := []struct {
		name       string
		input      SubmitPreflightInput
		allowed    bool
		submit     bool
		reasonWant string
	}{
		{
			name:    "under limit",
			input:   SubmitPreflightInput{TotalCost: 0.24, MaxSessionCost: 0.25},
			allowed: true,
			submit:  true,
		},
		{
			name:       "at limit",
			input:      SubmitPreflightInput{TotalCost: 0.25, MaxSessionCost: 0.25},
			reasonWant: "session cost limit reached ($0.2500/$0.2500)",
		},
		{
			name:       "over limit",
			input:      SubmitPreflightInput{TotalCost: 0.30, MaxSessionCost: 0.25},
			reasonWant: "session cost limit reached ($0.3000/$0.2500)",
		},
		{
			name:    "disabled",
			input:   SubmitPreflightInput{TotalCost: 100, MaxSessionCost: 0},
			allowed: true,
			submit:  true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := DecideSubmitPreflight(tt.input)
			if got.Allowed != tt.allowed || got.ShouldSubmit != tt.submit || got.Reason != tt.reasonWant {
				t.Fatalf("decision = %#v, want allowed=%v submit=%v reason=%q", got, tt.allowed, tt.submit, tt.reasonWant)
			}
		})
	}
}

func TestBudgetStopReasonOnlyStopsCrossedPositiveLimits(t *testing.T) {
	tests := []struct {
		name  string
		input BudgetStopInput
		want  string
	}{
		{
			name:  "under both",
			input: BudgetStopInput{CurrentTurnCost: 0.04, TotalCost: 0.40, MaxTurnCost: 0.05, MaxSessionCost: 0.50},
		},
		{
			name:  "turn limit",
			input: BudgetStopInput{CurrentTurnCost: 0.05, TotalCost: 0.40, MaxTurnCost: 0.05, MaxSessionCost: 0.50},
			want:  "turn cost limit reached ($0.0500/$0.0500)",
		},
		{
			name:  "session limit",
			input: BudgetStopInput{CurrentTurnCost: 0.04, TotalCost: 0.50, MaxTurnCost: 0.05, MaxSessionCost: 0.50},
			want:  "session cost limit reached ($0.5000/$0.5000)",
		},
		{
			name:  "disabled",
			input: BudgetStopInput{CurrentTurnCost: 10, TotalCost: 10},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := BudgetStopReason(tt.input); got != tt.want {
				t.Fatalf("BudgetStopReason(%#v) = %q, want %q", tt.input, got, tt.want)
			}
		})
	}
}

func TestMessageEndCrossingBudgetCancelsActiveTurn(t *testing.T) {
	m := readyModel(t)
	runner := &stubRunner{}
	m.Model.Runner = runner
	m.Model.Config = &config.Config{MaxTurnCost: 0.10}
	m.turnReducer().StartTurn(time.Now(), time.Now())
	m.InFlight.QueuedTurns = []string{"must not run"}

	next, cmd := m.handleMessageEnd(session.MessageEnd{
		Message: &session.AssistantMessage{
			Usage: session.Usage{Cost: session.Cost{Total: 0.10}},
		},
		Timestamp: time.Now(),
	})
	if cmd == nil {
		t.Fatal("budget stop did not schedule cancellation/persistence work")
	}
	if runner.aborts != 0 {
		t.Fatalf("abort ran during update = %d, want command-deferred abort", runner.aborts)
	}
	if next.Progress.Mode != StateCancelled {
		t.Fatalf("progress mode = %v, want canceled", next.Progress.Mode)
	}
	if next.Progress.BudgetStopReason != "turn cost limit reached ($0.1000/$0.1000)" {
		t.Fatalf("budget reason = %q", next.Progress.BudgetStopReason)
	}
	if len(next.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %#v, want cleared", next.InFlight.QueuedTurns)
	}
}

func TestBudgetReasonClearsAtNewTurnAndSessionReset(t *testing.T) {
	m := readyModel(t)
	m.Progress.BudgetStopReason = "old budget reason"
	m.turnReducer().StartTurn(time.Now(), time.Now())
	if m.Progress.BudgetStopReason != "" {
		t.Fatalf("new turn budget reason = %q, want empty", m.Progress.BudgetStopReason)
	}
	m.Progress.BudgetStopReason = "old budget reason"
	m.progressReducer().resetSessionUsage()
	if m.Progress.BudgetStopReason != "" {
		t.Fatalf("session reset budget reason = %q, want empty", m.Progress.BudgetStopReason)
	}
}
