package agent

import (
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestSessionAdapterNeedsCompaction(t *testing.T) {
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
			adapter := &SessionAdapter{
				config: &SessionAdapterConfig{
					Model: llm.Model{
						ContextWindow: tt.contextWindow,
					},
				},
				contextTokens: tt.contextTokens,
			}
			got := adapter.needsCompaction()
			if got != tt.expected {
				t.Fatalf("needsCompaction() = %v, want %v (window=%d, tokens=%d)",
					got, tt.expected, tt.contextWindow, tt.contextTokens)
			}
		})
	}
}

func TestSessionAdapterUpdateContextTokens(t *testing.T) {
	adapter := &SessionAdapter{}

	adapter.updateContextTokens(1000, 500)
	if adapter.contextTokens != 1500 {
		t.Fatalf("contextTokens = %d, want 1500", adapter.contextTokens)
	}

	adapter.updateContextTokens(2000, 1000)
	if adapter.contextTokens != 4500 {
		t.Fatalf("contextTokens = %d, want 4500", adapter.contextTokens)
	}
}

func TestSessionAdapterResetContextTokens(t *testing.T) {
	adapter := &SessionAdapter{
		contextTokens: 50000,
	}

	adapter.resetContextTokens()
	if adapter.contextTokens != 0 {
		t.Fatalf("contextTokens = %d, want 0", adapter.contextTokens)
	}
}

func TestSessionAdapterTracksTokenUsage(t *testing.T) {
	adapter := &SessionAdapter{
		config: &SessionAdapterConfig{
			ID: "test",
		},
		events: make(chan session.AgentEvent, 100),
	}

	// Simulate token usage event
	adapter.mu.Lock()
	adapter.updateContextTokens(10000, 5000)
	adapter.mu.Unlock()

	if adapter.contextTokens != 15000 {
		t.Fatalf("contextTokens = %d, want 15000", adapter.contextTokens)
	}
}
