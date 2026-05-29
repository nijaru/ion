package session

import (
	"context"
	"fmt"
	"strings"

	"github.com/nijaru/ion/llm"
)

const (
	branchSummaryPrefix = `The following is a summary of a branch that this conversation came back from:

<summary>
`
	branchSummarySuffix = `</summary>`
)

// BranchSummaryData records model-visible context for a branch that was left.
type BranchSummaryData struct {
	FromEventID string         `json:"from_event_id,omitzero"`
	Summary     string         `json:"summary"`
	Details     map[string]any `json:"details,omitzero"`
	FromHook    bool           `json:"from_hook,omitzero"`
}

// NewBranchSummaryEvent records a branch summary on the active branch.
func NewBranchSummaryEvent(sessionID string, summary BranchSummaryData) Event {
	summary = cloneBranchSummary(summary)
	return NewEvent(sessionID, BranchSummary, summary)
}

// BranchSummaryData decodes the payload of a branch-summary event.
func (e Event) BranchSummaryData() (BranchSummaryData, bool, error) {
	return decodeEventData[BranchSummaryData](e, BranchSummary, "branch summary")
}

// AppendBranchSummary appends a model-visible branch summary to the active branch.
func (s *Session) AppendBranchSummary(ctx context.Context, summary BranchSummaryData) error {
	if err := validateBranchSummary(summary); err != nil {
		return err
	}
	return s.Append(ctx, NewBranchSummaryEvent(s.ID(), summary))
}

// MoveLeafWithSummary moves the active branch and appends a summary there.
func (s *Session) MoveLeafWithSummary(
	ctx context.Context,
	eventID string,
	summary BranchSummaryData,
) error {
	if summary.FromEventID == "" {
		summary.FromEventID = eventID
	}
	if err := validateBranchSummary(summary); err != nil {
		return err
	}
	if err := s.MoveLeaf(ctx, eventID); err != nil {
		return err
	}
	return s.AppendBranchSummary(ctx, summary)
}

func validateBranchSummary(summary BranchSummaryData) error {
	if strings.TrimSpace(summary.Summary) == "" {
		return fmt.Errorf("session branch summary: summary is required")
	}
	return nil
}

func cloneBranchSummary(summary BranchSummaryData) BranchSummaryData {
	summary.Summary = strings.TrimSpace(summary.Summary)
	if len(summary.Details) > 0 {
		details := make(map[string]any, len(summary.Details))
		for key, value := range summary.Details {
			details[key] = value
		}
		summary.Details = details
	}
	return summary
}

func branchSummaryMessage(summary BranchSummaryData) llm.Message {
	return llm.TextMessage(
		llm.RoleUser,
		branchSummaryPrefix+strings.TrimSpace(summary.Summary)+"\n"+branchSummarySuffix,
	)
}
