package session

import (
	"strings"
	"time"

	"github.com/oklog/ulid/v2"
)

// Episode is a compressed record of a completed agent run.
// It captures only the signal: successful tool call pairs and the final
// conclusion. Orchestrators retrieve episodes from archival memory rather than
// full session logs, keeping coordination practical at scale.
type Episode struct {
	ID         string         `json:"id"`
	SessionID  string         `json:"session_id"`
	AgentID    string         `json:"agent_id"`
	StartTime  time.Time      `json:"start_time"`
	EndTime    time.Time      `json:"end_time"`
	Conclusion string         `json:"conclusion"` // last assistant message without tool calls
	Calls      []EpisodeCall  `json:"calls,omitzero"`
	TotalCost  float64        `json:"total_cost"`
	Metadata   map[string]any `json:"metadata,omitzero"`
}

// EpisodeCall is a single successful tool invocation captured in an Episode.
type EpisodeCall struct {
	Tool   string `json:"tool"`
	Args   string `json:"args"`
	Result string `json:"result"`
}

// Text returns the searchable text for this Episode: conclusion followed by tool names.
// Used as FTS5 content when storing in memory.
func (ep *Episode) Text() string {
	var sb strings.Builder
	sb.WriteString(ep.Conclusion)
	for _, c := range ep.Calls {
		sb.WriteByte(' ')
		sb.WriteString(c.Tool)
	}
	return sb.String()
}

// Distill compresses a RunLog into an Episode by extracting only the signal:
// successful tool call pairs and the final textual conclusion.
func Distill(traj *RunLog) *Episode {
	ep := &Episode{
		ID:        ulid.Make().String(),
		SessionID: traj.SessionID,
		AgentID:   traj.AgentID,
		StartTime: traj.StartTime,
		EndTime:   traj.EndTime,
		TotalCost: traj.TotalCost,
	}

	for _, turn := range traj.Turns {
		resultsByID := make(map[string]string, len(turn.ToolResults))
		for _, r := range turn.ToolResults {
			resultsByID[r.ToolID] = r.Content
		}

		for _, call := range turn.ToolCalls {
			result, ok := resultsByID[call.ID]
			if !ok {
				continue
			}
			ep.Calls = append(ep.Calls, EpisodeCall{
				Tool:   call.Function.Name,
				Args:   call.Function.Arguments,
				Result: result,
			})
		}

		if len(turn.ToolCalls) == 0 && turn.Output.Content != "" {
			ep.Conclusion = turn.Output.Content
		}
	}

	return ep
}
