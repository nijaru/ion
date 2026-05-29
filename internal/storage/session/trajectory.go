package session

import (
	"time"

	"github.com/nijaru/ion/internal/llm"
)

// RunLog represents a structured trace of an agent's execution.
// It is used for evaluation, reinforcement learning (RL) fine-tuning,
// and offline analysis.
type RunLog struct {
	SessionID string         `json:"session_id"`
	AgentID   string         `json:"agent_id"`
	StartTime time.Time      `json:"start_time"`
	EndTime   time.Time      `json:"end_time"`
	Turns     []RunTurn      `json:"turns"`
	ChildRuns []ChildRunLog  `json:"child_runs,omitzero"`
	TotalCost float64        `json:"total_cost"`
	Metadata  map[string]any `json:"metadata,omitzero"`
}

// ChildRunLog records a child run linked from a parent session.
type ChildRunLog struct {
	ChildID   string         `json:"child_id"`
	SessionID string         `json:"session_id"`
	AgentID   string         `json:"agent_id"`
	Mode      ChildMode      `json:"mode"`
	Status    ChildStatus    `json:"status"`
	Summary   string         `json:"summary,omitzero"`
	Artifacts []ArtifactRef  `json:"artifacts,omitzero"`
	Run       *RunLog        `json:"run,omitzero"`
	Metadata  map[string]any `json:"metadata,omitzero"`
}

// RunTurn represents a single perceive-decide-act-observe loop.
type RunTurn struct {
	TurnID       string         `json:"turn_id"`
	Timestamp    time.Time      `json:"timestamp"`
	Input        []llm.Message  `json:"input"`
	InputEntries []HistoryEntry `json:"input_entries,omitzero"`
	Output       llm.Message    `json:"output"`
	ToolCalls    []llm.Call     `json:"tool_calls,omitzero"`
	ToolResults  []llm.Message  `json:"tool_results,omitzero"`
	Cost         float64        `json:"cost"`
	Metrics      map[string]any `json:"metrics,omitzero"`
}
