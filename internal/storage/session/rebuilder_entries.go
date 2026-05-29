package session

import (
	"strings"

	"github.com/nijaru/ion/internal/llm"
)

func normalizeEffectiveEntries(entries []HistoryEntry) []HistoryEntry {
	out := entries[:0]
	pending := make(map[string]int)
	for _, entry := range entries {
		entry, ok := normalizeEffectiveEntry(entry)
		if !ok {
			continue
		}
		msg := entry.Message
		if msg.Role == llm.RoleTool {
			if msg.ToolID == "" || pending[msg.ToolID] == 0 {
				continue
			}
			pending[msg.ToolID]--
			if pending[msg.ToolID] == 0 {
				delete(pending, msg.ToolID)
			}
			out = append(out, entry)
			continue
		}
		clear(pending)
		out = append(out, entry)
		if msg.Role == llm.RoleAssistant {
			addPendingToolCalls(pending, msg.Calls)
		}
	}
	return out
}

func normalizeEffectiveEntry(entry HistoryEntry) (HistoryEntry, bool) {
	entry.Message = normalizeTranscriptMessage(entry.Message)
	entry.Tool = normalizeToolHistory(entry.Message, entry.Tool)
	if entry.EventType == "" {
		inferLegacyContextMarkers(&entry)
	}
	if entry.EventType == ContextAdded && entry.ContextKind == "" {
		entry.ContextKind = ContextKindGeneric
	}
	if entry.EventType == ContextAdded && entry.ContextPlacement == "" {
		contextEntry := ContextEntry{Kind: entry.ContextKind}
		normalizeContextEntry(&contextEntry)
		entry.ContextPlacement = contextEntry.Placement
	}
	if entry.EventType != ContextAdded && !validModelMessage(entry.Message) {
		return HistoryEntry{}, false
	}
	return entry, true
}

func normalizeToolHistory(msg llm.Message, tool *ToolHistory) *ToolHistory {
	if msg.Role != llm.RoleTool || msg.ToolID == "" || tool == nil {
		return nil
	}
	if tool.ID != "" && tool.ID != msg.ToolID {
		return nil
	}
	normalized := *tool
	if normalized.ID == "" {
		normalized.ID = msg.ToolID
	}
	if normalized.Name == "" {
		normalized.Name = msg.Name
	}
	return &normalized
}

func validModelMessage(msg llm.Message) bool {
	if msg.Role != llm.RoleSystem &&
		msg.Role != llm.RoleDeveloper &&
		msg.Role != llm.RoleUser &&
		msg.Role != llm.RoleAssistant &&
		msg.Role != llm.RoleTool {
		return false
	}
	if msg.Role != llm.RoleAssistant {
		return true
	}
	return assistantMessageHasPayload(msg)
}

func assistantMessageHasPayload(msg llm.Message) bool {
	return msg.HasAssistantPayload()
}

func inferLegacyContextMarkers(entry *HistoryEntry) {
	switch {
	case strings.Contains(entry.Message.Content, "<conversation_summary>"):
		entry.EventType = ContextAdded
		entry.ContextKind = ContextKindSummary
		entry.ContextPlacement = ContextPlacementPrefix
	case strings.Contains(entry.Message.Content, "<working_set>"):
		entry.EventType = ContextAdded
		entry.ContextKind = ContextKindWorkingSet
		entry.ContextPlacement = ContextPlacementPrefix
	}
}

func arrangePromptEntries(entries []HistoryEntry) []HistoryEntry {
	prefixCount := 0
	for _, entry := range entries {
		if isPrefixContextEntry(entry) {
			prefixCount++
		}
	}
	if prefixCount == 0 {
		return entries
	}

	out := make([]HistoryEntry, 0, len(entries))
	for _, entry := range entries {
		if isPrefixContextEntry(entry) {
			out = append(out, entry)
		}
	}
	for _, entry := range entries {
		if !isPrefixContextEntry(entry) {
			out = append(out, entry)
		}
	}
	return out
}

func normalizeTranscriptMessage(msg llm.Message) llm.Message {
	if msg.Role != llm.RoleSystem && msg.Role != llm.RoleDeveloper {
		return msg
	}
	msg.Role = llm.RoleUser
	msg.CacheControl = nil
	return msg
}
