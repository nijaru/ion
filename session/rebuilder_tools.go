package session

import (
	"fmt"
	"strings"

	"github.com/nijaru/ion/llm"
)

type toolLifecycle struct {
	started       ToolStartedData
	completed     ToolCompletedData
	completedID   string
	id            string
	startIndex    int
	completeIndex int
	hasStarted    bool
	hasCompleted  bool
}

type toolLifecycleIndex struct {
	records      []toolLifecycle
	eventIndexes map[string]int
	eventsLen    int
}

type toolLifecycleMatcher struct {
	index          *toolLifecycleIndex
	assistantIndex int
	boundaryIndex  int
	used           map[int]struct{}
}

func recoverCompletedToolResults(entries []HistoryEntry, events []Event) ([]HistoryEntry, error) {
	lifecycle, err := newToolLifecycleIndex(events)
	if err != nil {
		return nil, err
	}

	out := make([]HistoryEntry, 0, len(entries))
	for i := 0; i < len(entries); {
		entry := entries[i]
		msg := normalizeTranscriptMessage(entry.Message)
		if msg.Role != llm.RoleAssistant || len(msg.Calls) == 0 {
			out = append(out, entry)
			i++
			continue
		}

		existing := make(map[string][]HistoryEntry)
		j := i + 1
		for j < len(entries) {
			next := entries[j]
			next.Message = normalizeTranscriptMessage(next.Message)
			if next.Message.Role != llm.RoleTool {
				break
			}
			if next.Message.ToolID != "" {
				existing[next.Message.ToolID] = append(existing[next.Message.ToolID], next)
			}
			j++
		}

		assistant := entry
		matcher := lifecycle.matcher(entry, entries, j)
		keptCalls := make([]llm.Call, 0, len(msg.Calls))
		toolEntries := make([]HistoryEntry, 0, len(msg.Calls))
		for _, call := range msg.Calls {
			if call.ID == "" {
				continue
			}
			queue := existing[call.ID]
			if len(queue) > 0 {
				toolEntries = append(toolEntries, queue[0])
				existing[call.ID] = queue[1:]
				keptCalls = append(keptCalls, call)
				matcher.consume(call.ID, false)
				continue
			}
			record, ok := matcher.consume(call.ID, true)
			if !ok {
				continue
			}
			toolEntries = append(toolEntries, toolEntryFromLifecycle(call, record))
			keptCalls = append(keptCalls, call)
		}
		assistant.Message.Calls = keptCalls
		out = append(out, assistant)
		out = append(out, toolEntries...)
		i = j
	}
	return out, nil
}

func toolEntryFromLifecycle(call llm.Call, record toolLifecycle) HistoryEntry {
	name := record.completed.Tool
	if name == "" {
		name = record.started.Tool
	}
	if name == "" {
		name = call.Function.Name
	}
	content := record.completed.Output
	if record.completed.Error != "" && !strings.Contains(content, record.completed.Error) {
		content = strings.TrimSpace(
			strings.TrimSpace(content) + "\n" + fmt.Sprintf("Error: %s", record.completed.Error),
		)
	}
	tool := mergeToolHistory(nil, llm.Message{
		Role:   llm.RoleTool,
		ToolID: call.ID,
		Name:   name,
	}, record)
	return HistoryEntry{
		EventID:   record.completedID,
		EventType: ToolCompleted,
		Message: llm.Message{
			Role:    llm.RoleTool,
			ToolID:  call.ID,
			Name:    name,
			Content: content,
			Parts:   append([]llm.ContentPart(nil), record.completed.Parts...),
		},
		Tool: &tool,
	}
}

func newToolLifecycleIndex(events []Event) (*toolLifecycleIndex, error) {
	index := &toolLifecycleIndex{
		eventIndexes: make(map[string]int, len(events)),
		eventsLen:    len(events),
	}
	for i := range events {
		e := &events[i]
		index.eventIndexes[e.ID.String()] = i
		switch e.Type {
		case ToolStarted:
			data, ok, err := e.ToolStartedData()
			if err != nil {
				return nil, err
			}
			if !ok || data.ID == "" {
				continue
			}
			index.records = append(index.records, toolLifecycle{
				id:         data.ID,
				started:    data,
				startIndex: i,
				hasStarted: true,
			})
		case ToolCompleted:
			data, ok, err := e.ToolCompletedData()
			if err != nil {
				return nil, err
			}
			if !ok || data.ID == "" {
				continue
			}
			recordIndex := index.latestOpenRecord(data.ID)
			if recordIndex < 0 {
				index.records = append(index.records, toolLifecycle{
					id:            data.ID,
					startIndex:    -1,
					completeIndex: i,
					completed:     data,
					completedID:   e.ID.String(),
					hasCompleted:  true,
				})
				continue
			}
			record := index.records[recordIndex]
			record.completed = data
			record.completedID = e.ID.String()
			record.completeIndex = i
			record.hasCompleted = true
			index.records[recordIndex] = record
		}
	}
	return index, nil
}

func (idx *toolLifecycleIndex) latestOpenRecord(id string) int {
	for i := len(idx.records) - 1; i >= 0; i-- {
		record := idx.records[i]
		if record.id == id && !record.hasCompleted {
			return i
		}
	}
	return -1
}

func (idx *toolLifecycleIndex) matcher(
	assistant HistoryEntry,
	entries []HistoryEntry,
	boundaryEntryIndex int,
) *toolLifecycleMatcher {
	assistantIndex := idx.entryIndex(assistant)
	boundaryIndex := -1
	if assistantIndex >= 0 {
		boundaryIndex = idx.eventsLen
		if boundaryEntryIndex < len(entries) {
			if next := idx.entryIndex(entries[boundaryEntryIndex]); next >= 0 {
				boundaryIndex = next
			}
		}
	}
	return &toolLifecycleMatcher{
		index:          idx,
		assistantIndex: assistantIndex,
		boundaryIndex:  boundaryIndex,
		used:           make(map[int]struct{}),
	}
}

func (idx *toolLifecycleIndex) entryIndex(entry HistoryEntry) int {
	if entry.EventID == "" {
		return -1
	}
	eventIndex, ok := idx.eventIndexes[entry.EventID]
	if !ok {
		return -1
	}
	return eventIndex
}

func (m *toolLifecycleMatcher) consume(
	id string,
	requireCompleted bool,
) (toolLifecycle, bool) {
	if m == nil || m.assistantIndex < 0 || id == "" {
		return toolLifecycle{}, false
	}
	for i, record := range m.index.records {
		if _, ok := m.used[i]; ok || record.id != id {
			continue
		}
		if requireCompleted && !record.hasCompleted {
			continue
		}
		if !m.inScope(record) {
			continue
		}
		m.used[i] = struct{}{}
		return record, true
	}
	return toolLifecycle{}, false
}

func (m *toolLifecycleMatcher) inScope(record toolLifecycle) bool {
	first, last := recordRange(record)
	if first < 0 || last < 0 {
		return false
	}
	return first > m.assistantIndex && last < m.boundaryIndex
}

func recordRange(record toolLifecycle) (int, int) {
	first := record.startIndex
	last := record.startIndex
	if !record.hasStarted {
		first = record.completeIndex
		last = record.completeIndex
	}
	if record.hasCompleted {
		last = record.completeIndex
	}
	return first, last
}

func withToolHistory(entries []HistoryEntry, events []Event) ([]HistoryEntry, error) {
	lifecycle, err := newToolLifecycleIndex(events)
	if err != nil {
		return nil, err
	}
	if len(lifecycle.records) == 0 {
		return entries, nil
	}

	for i := 0; i < len(entries); {
		entry := entries[i]
		msg := normalizeTranscriptMessage(entry.Message)
		if msg.Role != llm.RoleAssistant || len(msg.Calls) == 0 {
			i++
			continue
		}

		j := i + 1
		for j < len(entries) {
			next := normalizeTranscriptMessage(entries[j].Message)
			if next.Role != llm.RoleTool {
				break
			}
			j++
		}

		matcher := lifecycle.matcher(entry, entries, j)
		for k := i + 1; k < j; k++ {
			if entries[k].Message.ToolID == "" {
				continue
			}
			record, ok := matcher.consume(entries[k].Message.ToolID, false)
			if !ok && entries[k].Tool == nil {
				continue
			}
			tool := mergeToolHistory(entries[k].Tool, entries[k].Message, record)
			entries[k].Tool = &tool
		}
		i = j
	}
	return entries, nil
}

func mergeToolHistory(existing *ToolHistory, msg llm.Message, record toolLifecycle) ToolHistory {
	var tool ToolHistory
	if existing != nil {
		tool = *existing
	}
	if tool.ID == "" {
		tool.ID = msg.ToolID
	}
	if tool.Name == "" {
		tool.Name = msg.Name
	}
	if record.hasStarted {
		if tool.Name == "" {
			tool.Name = record.started.Tool
		}
		if tool.Arguments == "" {
			tool.Arguments = record.started.Arguments
		}
		if tool.IdempotencyKey == "" {
			tool.IdempotencyKey = record.started.IdempotencyKey
		}
	}
	if record.hasCompleted {
		if tool.Name == "" {
			tool.Name = record.completed.Tool
		}
		if tool.IdempotencyKey == "" {
			tool.IdempotencyKey = record.completed.IdempotencyKey
		}
		if record.completed.Error != "" {
			tool.IsError = true
			if tool.Error == "" {
				tool.Error = record.completed.Error
			}
		}
	}
	return tool
}

func pendingToolCalls(events []Event) (map[string]int, error) {
	pending := make(map[string]int)
	for i := range events {
		e := &events[i]
		if e.Type != MessageAdded {
			continue
		}
		msg, err := e.ensureMessage()
		if err != nil {
			return nil, err
		}
		switch msg.Role {
		case llm.RoleAssistant:
			clear(pending)
			addPendingToolCalls(pending, msg.Calls)
		case llm.RoleTool:
			if msg.ToolID == "" || pending[msg.ToolID] == 0 {
				continue
			}
			pending[msg.ToolID]--
			if pending[msg.ToolID] == 0 {
				delete(pending, msg.ToolID)
			}
		default:
			clear(pending)
		}
	}
	return pending, nil
}

func addPendingToolCalls(pending map[string]int, calls []llm.Call) {
	for _, call := range calls {
		if call.ID == "" {
			continue
		}
		pending[call.ID]++
	}
}
