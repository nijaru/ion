package session

import "github.com/nijaru/ion/llm"

// ExportRun converts a session's event log into a structured RunLog.
func ExportRun(sess *Session) (*RunLog, error) {
	return exportRun(sess)
}

func exportRun(sess *Session) (*RunLog, error) {
	sess.mu.Lock()
	defer sess.mu.Unlock()

	events := sess.events
	if len(events) == 0 {
		return &RunLog{
			SessionID: sess.ID(),
			Turns:     []RunTurn{},
		}, nil
	}

	traj := &RunLog{
		SessionID: sess.ID(),
		StartTime: events[0].Timestamp,
		EndTime:   events[len(events)-1].Timestamp,
		Metadata:  make(map[string]any),
		Turns:     make([]RunTurn, 0, len(events)/4+1),
	}

	var currentTurn *RunTurn
	var inputBuffer []HistoryEntry

	for i := range events {
		e := &events[i]
		traj.TotalCost += e.Cost

		switch e.Type {
		case ContextAdded:
			entry, err := sess.historyEntryFromEvent(e)
			if err != nil {
				continue
			}
			inputBuffer = append(inputBuffer, entry)
		case MessageAdded:
			entry, err := sess.historyEntryFromEvent(e)
			if err != nil {
				continue
			}
			msg := entry.Message

			switch msg.Role {
			case llm.RoleUser, llm.RoleSystem, llm.RoleDeveloper:
				inputBuffer = append(inputBuffer, entry)
			case llm.RoleAssistant:
				if currentTurn != nil {
					traj.Turns = append(traj.Turns, *currentTurn)
				}
				currentTurn = &RunTurn{
					TurnID:       e.ID.String(),
					Timestamp:    e.Timestamp,
					Input:        historyEntriesToMessages(inputBuffer),
					InputEntries: append([]HistoryEntry(nil), inputBuffer...),
					Output:       msg,
					ToolCalls:    msg.Calls,
					Cost:         e.Cost,
				}
				inputBuffer = inputBuffer[:0]
			case llm.RoleTool:
				if currentTurn == nil {
					continue
				}
				currentTurn.ToolResults = append(currentTurn.ToolResults, msg)
				inputBuffer = append(inputBuffer, entry)
			}
		}
	}

	if currentTurn != nil {
		traj.Turns = append(traj.Turns, *currentTurn)
	}

	return traj, nil
}

func historyEntriesToMessages(entries []HistoryEntry) []llm.Message {
	messages := make([]llm.Message, 0, len(entries))
	for _, entry := range entries {
		messages = append(messages, entry.Message)
	}
	return messages
}
