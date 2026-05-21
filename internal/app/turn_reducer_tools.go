package app

import (
	"time"

	"github.com/nijaru/ion/internal/session"
)

func (r turnReducer) appendToolOutput(toolUseID, delta string) {
	if entry := r.pendingToolEntry(toolUseID); entry != nil {
		entry.Content += delta
	}
}

func (r turnReducer) startToolCall(
	toolUseID string,
	timestamp time.Time,
	title string,
) string {
	r.progress.Mode = stateWorking
	r.progress.LastToolUseID = toolUseID
	if r.progress.LastToolUseID == "" {
		r.progress.LastToolUseID = session.ShortID()
	}
	entry := &session.Entry{
		Role:      session.Tool,
		Timestamp: timestamp,
		Title:     title,
	}
	if r.inFlight.PendingTools == nil {
		r.inFlight.PendingTools = make(map[string]*session.Entry)
	}
	r.inFlight.PendingTools[r.progress.LastToolUseID] = entry
	if r.inFlight.Pending == nil || r.inFlight.Pending.Role == session.Tool ||
		(r.inFlight.Pending.Role == session.Agent &&
			r.inFlight.Pending.Content == "" &&
			r.inFlight.ReasonBuf == "") {
		r.inFlight.Pending = entry
	}
	return r.progress.LastToolUseID
}

func (r turnReducer) completeToolResult(
	toolUseID string,
	msg session.ToolResult,
) (session.Entry, bool) {
	pending := r.pendingToolEntry(toolUseID)
	if pending == nil {
		return session.Entry{}, false
	}
	pending.Content = msg.Result
	pending.IsError = msg.Error != nil
	setEntryTimestamp(pending, msg.Timestamp)
	entry := *pending
	r.clearPendingTool(toolUseID, pending)
	if len(r.inFlight.PendingTools) == 0 {
		r.progress.Mode = stateIonizing
		r.progress.Status = ""
		r.progress.ContextTokens = 0
	}
	return entry, true
}

func (r turnReducer) pendingToolEntry(toolUseID string) *session.Entry {
	if toolUseID != "" {
		return r.inFlight.PendingTools[toolUseID]
	}
	if r.inFlight.Pending != nil && r.inFlight.Pending.Role == session.Tool {
		return r.inFlight.Pending
	}
	return nil
}

func (r turnReducer) clearPendingTool(toolUseID string, entry *session.Entry) {
	if toolUseID != "" {
		delete(r.inFlight.PendingTools, toolUseID)
	}
	if len(r.inFlight.PendingTools) == 0 {
		r.inFlight.PendingTools = nil
	}
	if r.inFlight.Pending == entry {
		r.inFlight.Pending = nil
		for _, id := range sortedKeys(r.inFlight.PendingTools) {
			r.inFlight.Pending = r.inFlight.PendingTools[id]
			break
		}
	}
}
