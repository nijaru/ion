package session

import (
	"fmt"

	"github.com/nijaru/ion/internal/llm"
)

// ToolCompletedData captures the durable outcome of a completed tool call.
type ToolCompletedData struct {
	Tool           string            `json:"tool"`
	ID             string            `json:"id"`
	IdempotencyKey string            `json:"idempotency_key,omitzero"`
	Output         string            `json:"output,omitzero"`
	Parts          []llm.ContentPart `json:"parts,omitzero"`
	Error          string            `json:"error,omitzero"`
}

// NewToolCompletedEvent records the durable result of a tool execution.
func NewToolCompletedEvent(sessionID string, result ToolCompletedData) Event {
	return NewEvent(sessionID, ToolCompleted, result)
}

// ToolCompletedData decodes the payload of a tool-completed event.
func (e Event) ToolCompletedData() (ToolCompletedData, bool, error) {
	if e.Type != ToolCompleted {
		return ToolCompletedData{}, false, nil
	}

	var result ToolCompletedData
	if err := e.UnmarshalData(&result); err != nil {
		return ToolCompletedData{}, true, fmt.Errorf(
			"decode tool completed event %s: %w",
			e.ID,
			err,
		)
	}
	return result, true, nil
}

// ToolExecutionRecord summarizes durable tool lifecycle facts for one
// idempotency key.
type ToolExecutionRecord struct {
	Started   ToolStartedData
	Completed ToolCompletedData
}

// FindToolExecutionByKey looks up the most recent durable tool lifecycle facts
// for an idempotency key.
func FindToolExecutionByKey(
	s *Session,
	idempotencyKey string,
) (ToolExecutionRecord, bool, error) {
	if s == nil || idempotencyKey == "" {
		return ToolExecutionRecord{}, false, nil
	}

	var record ToolExecutionRecord
	for e := range s.Backward() {
		switch e.Type {
		case ToolCompleted:
			data, ok, err := e.ToolCompletedData()
			if err != nil {
				return ToolExecutionRecord{}, false, err
			}
			if ok && data.IdempotencyKey == idempotencyKey {
				record.Completed = data
				return record, true, nil
			}
		case ToolStarted:
			data, ok, err := e.ToolStartedData()
			if err != nil {
				return ToolExecutionRecord{}, false, err
			}
			if ok && data.IdempotencyKey == idempotencyKey {
				record.Started = data
				return record, true, nil
			}
		}
	}
	return ToolExecutionRecord{}, false, nil
}
