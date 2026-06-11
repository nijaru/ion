package agent

import (
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestAgentNeedsCompaction(t *testing.T) {
	tests := []struct {
		name           string
		contextWindow  int
		contextTokens int
		expected       bool
	}{
		{"no_window", 0, 100000, false},
		{"under_threshold", 100000, 70000, false},
		{"at_threshold", 100000, 80000, false},
		{"over_threshold", 100000, 85000, true},
		{"well_over", 100000, 150000, true},
		{"small_window", 10000, 9000, true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			a := &Agent{
				config: AgentConfig{
					Model: llm.Model{
						ContextWindow: tt.contextWindow,
					},
				},
				contextTokens: tt.contextTokens,
			}
			got := a.needsCompaction()
			if got != tt.expected {
				t.Fatalf("needsCompaction() = %v, want %v (window=%d, tokens=%d)",
					got, tt.expected, tt.contextWindow, tt.contextTokens)
			}
		})
	}
}

func TestAgentUpdateContextTokens(t *testing.T) {
	a := &Agent{}

	a.updateContextTokens(1000, 500)
	if a.contextTokens != 1500 {
		t.Fatalf("contextTokens = %d, want 1500", a.contextTokens)
	}

	a.updateContextTokens(2000, 1000)
	if a.contextTokens != 4500 {
		t.Fatalf("contextTokens = %d, want 4500", a.contextTokens)
	}
}

func TestAgentResetContextTokens(t *testing.T) {
	a := &Agent{
		contextTokens: 50000,
	}

	a.resetContextTokens()
	if a.contextTokens != 0 {
		t.Fatalf("contextTokens = %d, want 0", a.contextTokens)
	}
}

func TestAgentTracksTokenUsage(t *testing.T) {
	a := &Agent{
		config: AgentConfig{
			ID: "test",
		},
		events: make(chan session.AgentEvent, 100),
	}

	// Simulate token usage event
	a.mu.Lock()
	a.updateContextTokens(10000, 5000)
	a.mu.Unlock()

	if a.contextTokens != 15000 {
		t.Fatalf("contextTokens = %d, want 15000", a.contextTokens)
	}
}
