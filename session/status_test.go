package session

import (
	"testing"
	"time"
)

func TestDecideStatusChange(t *testing.T) {
	now := time.Date(2026, 5, 31, 2, 0, 0, 0, time.UTC)
	eventTS := time.Date(2026, 5, 31, 1, 0, 0, 0, time.UTC)

	tests := []struct {
		name  string
		input StatusChangeInput
		want  StatusChangeDecision
	}{
		{
			name: "subagent status is not root status",
			input: StatusChangeInput{
				AgentID:   "worker",
				Status:    "Running",
				Timestamp: eventTS,
				Now:       now,
			},
			want: StatusChangeDecision{},
		},
		{
			name: "root status preserves event timestamp",
			input: StatusChangeInput{
				Status:    "Thinking...",
				Timestamp: eventTS,
				Now:       now,
			},
			want: StatusChangeDecision{
				Root:             true,
				Status:           "Thinking...",
				StatusUpdatedAt:  eventTS,
				PersistTimestamp: eventTS,
			},
		},
		{
			name: "root status uses now for display timestamp fallback",
			input: StatusChangeInput{
				Status: "Thinking...",
				Now:    now,
			},
			want: StatusChangeDecision{
				Root:            true,
				Status:          "Thinking...",
				StatusUpdatedAt: now,
			},
		},
		{
			name: "compaction status",
			input: StatusChangeInput{
				Status: "Compacting context...",
				Now:    now,
			},
			want: StatusChangeDecision{
				Root:            true,
				Status:          "Compacting context...",
				StatusUpdatedAt: now,
				Compacting:      true,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := DecideStatusChange(tt.input); got != tt.want {
				t.Fatalf("DecideStatusChange() = %#v, want %#v", got, tt.want)
			}
		})
	}
}

func TestIsCompactingStatus(t *testing.T) {
	tests := []struct {
		status string
		want   bool
	}{
		{status: "", want: false},
		{status: "Ready", want: false},
		{status: "Compacting context...", want: true},
		{status: " auto-COMPACTING history ", want: true},
	}

	for _, tt := range tests {
		if got := IsCompactingStatus(tt.status); got != tt.want {
			t.Fatalf("IsCompactingStatus(%q) = %v, want %v", tt.status, got, tt.want)
		}
	}
}
