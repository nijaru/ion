package session

import (
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
)

func TestDistill(t *testing.T) {
	traj := &RunLog{
		SessionID: "sess-1",
		AgentID:   "agent-1",
		StartTime: time.Now(),
		EndTime:   time.Now().Add(time.Minute),
		TotalCost: 0.05,
		Turns: []RunTurn{
			{
				// Turn with a successful tool call
				Output: llm.Message{
					Role:    llm.RoleAssistant,
					Content: "Let me search for that.",
					Calls: []llm.Call{
						{ID: "c1", Function: struct {
							Name      string `json:"name"`
							Arguments string `json:"arguments"`
						}{Name: "search", Arguments: `{"q":"golang"}`}},
					},
				},
				ToolCalls: []llm.Call{
					{ID: "c1", Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "search", Arguments: `{"q":"golang"}`}},
				},
				ToolResults: []llm.Message{
					{
						Role:    llm.RoleTool,
						ToolID:  "c1",
						Content: "Go is a statically typed language.",
					},
				},
			},
			{
				// Turn with an unpaired tool call (no result) — must be skipped
				ToolCalls: []llm.Call{
					{ID: "c2", Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "bash", Arguments: `{}`}},
				},
				ToolResults: []llm.Message{},
			},
			{
				// Final conclusion turn (no tool calls)
				Output: llm.Message{
					Role:    llm.RoleAssistant,
					Content: "Go is a statically typed, compiled language developed at Google.",
				},
			},
		},
	}

	ep := Distill(traj)

	if ep.ID == "" {
		t.Error("Episode ID must not be empty")
	}
	if ep.SessionID != "sess-1" {
		t.Errorf("SessionID: got %q, want %q", ep.SessionID, "sess-1")
	}
	if ep.AgentID != "agent-1" {
		t.Errorf("AgentID: got %q, want %q", ep.AgentID, "agent-1")
	}
	if ep.TotalCost != 0.05 {
		t.Errorf("TotalCost: got %f, want 0.05", ep.TotalCost)
	}

	if len(ep.Calls) != 1 {
		t.Fatalf("Calls: got %d, want 1 (unpaired call must be skipped)", len(ep.Calls))
	}
	call := ep.Calls[0]
	if call.Tool != "search" {
		t.Errorf("Calls[0].Tool: got %q, want %q", call.Tool, "search")
	}
	if call.Result != "Go is a statically typed language." {
		t.Errorf("Calls[0].Result: got %q", call.Result)
	}

	want := "Go is a statically typed, compiled language developed at Google."
	if ep.Conclusion != want {
		t.Errorf("Conclusion: got %q, want %q", ep.Conclusion, want)
	}
}

func TestDistill_Empty(t *testing.T) {
	ep := Distill(&RunLog{SessionID: "s", AgentID: "a"})
	if ep.ID == "" {
		t.Error("Episode ID must not be empty even for empty trajectory")
	}
	if len(ep.Calls) != 0 {
		t.Errorf("expected 0 calls, got %d", len(ep.Calls))
	}
	if ep.Conclusion != "" {
		t.Errorf("expected empty conclusion, got %q", ep.Conclusion)
	}
}

func TestEpisode_Text(t *testing.T) {
	ep := &Episode{
		Conclusion: "The answer is 42.",
		Calls: []EpisodeCall{
			{Tool: "search"},
			{Tool: "bash"},
		},
	}
	got := ep.Text()
	if got != "The answer is 42. search bash" {
		t.Errorf("Text(): got %q", got)
	}
}
