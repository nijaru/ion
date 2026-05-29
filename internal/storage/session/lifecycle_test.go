package session

import (
	"testing"

	"github.com/nijaru/ion/internal/llm"
)

func TestLifecycleEventsRoundTrip(t *testing.T) {
	step := NewStepStartedEvent("sess", StepStartedData{
		AgentID: "agent",
		Model:   "model",
		PromptCache: PromptCacheData{
			PrefixHash:     "prefix",
			ToolSchemaHash: "tools",
		},
	})
	data, ok, err := step.StepStartedData()
	if err != nil {
		t.Fatalf("decode step started: %v", err)
	}
	if !ok {
		t.Fatal("expected step started payload")
	}
	if data.AgentID != "agent" || data.PromptCache.PrefixHash != "prefix" {
		t.Fatalf("unexpected step started payload: %+v", data)
	}

	turn := NewTurnCompletedEvent("sess", TurnCompletedData{
		AgentID:        "agent",
		Steps:          3,
		Usage:          llm.Usage{InputTokens: 1, OutputTokens: 2, TotalTokens: 3},
		TurnStopReason: "completed",
		Error:          "boom",
	})
	turnData, ok, err := turn.TurnCompletedData()
	if err != nil {
		t.Fatalf("decode turn completed: %v", err)
	}
	if !ok {
		t.Fatal("expected turn completed payload")
	}
	if turnData.Steps != 3 || turnData.TurnStopReason != "completed" || turnData.Error != "boom" {
		t.Fatalf("unexpected turn completed payload: %+v", turnData)
	}

	tool := NewToolStartedEvent("sess", ToolStartedData{
		Tool:           "read",
		Arguments:      "{}",
		ID:             "call-1",
		IdempotencyKey: "sess:step:read:0:hash",
	})
	toolData, ok, err := tool.ToolStartedData()
	if err != nil {
		t.Fatalf("decode tool started: %v", err)
	}
	if !ok {
		t.Fatal("expected tool started payload")
	}
	if toolData.Tool != "read" || toolData.ID != "call-1" ||
		toolData.IdempotencyKey != "sess:step:read:0:hash" {
		t.Fatalf("unexpected tool started payload: %+v", toolData)
	}

	completed := NewToolCompletedEvent("sess", ToolCompletedData{
		Tool:           "read",
		ID:             "call-1",
		IdempotencyKey: "sess:step:read:0:hash",
		Output:         "ok",
		Error:          "failed",
	})
	completedData, ok, err := completed.ToolCompletedData()
	if err != nil {
		t.Fatalf("decode tool completed: %v", err)
	}
	if !ok {
		t.Fatal("expected tool completed payload")
	}
	if completedData.IdempotencyKey != "sess:step:read:0:hash" ||
		completedData.Output != "ok" ||
		completedData.Error != "failed" {
		t.Fatalf("unexpected tool completed payload: %+v", completedData)
	}

	compaction := NewCompactionStartedEvent("sess", CompactionStartedData{
		Strategy:      "summarize",
		MaxTokens:     1000,
		ThresholdPct:  0.75,
		CurrentTokens: 1500,
	})
	compactionData, ok, err := compaction.CompactionStartedData()
	if err != nil {
		t.Fatalf("decode compaction started: %v", err)
	}
	if !ok {
		t.Fatal("expected compaction started payload")
	}
	if compactionData.Strategy != "summarize" || compactionData.CurrentTokens != 1500 {
		t.Fatalf("unexpected compaction started payload: %+v", compactionData)
	}

	retry := NewEscalationRetriedEvent("sess", EscalationRetriedData{
		AgentID: "agent",
		Scope:   "model",
		Target:  "gpt-test",
		Attempt: 2,
		Error:   "transient provider failure",
	})
	retryData, ok, err := retry.EscalationRetriedData()
	if err != nil {
		t.Fatalf("decode escalation retried: %v", err)
	}
	if !ok {
		t.Fatal("expected escalation retried payload")
	}
	if retryData.Attempt != 2 || retryData.Scope != "model" {
		t.Fatalf("unexpected escalation retried payload: %+v", retryData)
	}
}
