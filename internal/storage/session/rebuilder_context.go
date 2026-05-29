package session

import (
	"slices"
	"strings"
)

func (r *Rebuilder) fileContextEntry(snapshot CompactionSnapshot) (HistoryEntry, bool) {
	limit := r.FilesLimit
	if limit <= 0 {
		limit = defaultRebuilderFilesLimit
	}

	modified := uniqueSorted(snapshot.ModifiedFiles)
	readOnly := subtract(uniqueSorted(snapshot.ReadFiles), modified)
	if len(modified) == 0 && len(readOnly) == 0 {
		return HistoryEntry{}, false
	}

	var sb strings.Builder
	sb.WriteString("<working_set>\n")
	if len(modified) > 0 {
		sb.WriteString("Modified files:\n")
		for _, path := range modified[:min(limit, len(modified))] {
			sb.WriteString("- ")
			sb.WriteString(path)
			sb.WriteByte('\n')
		}
	}
	if len(readOnly) > 0 {
		sb.WriteString("Read-only files:\n")
		for _, path := range readOnly[:min(limit, len(readOnly))] {
			sb.WriteString("- ")
			sb.WriteString(path)
			sb.WriteByte('\n')
		}
	}
	sb.WriteString("</working_set>")

	return HistoryEntry{
		EventType:        ContextAdded,
		ContextKind:      ContextKindWorkingSet,
		ContextPlacement: ContextPlacementPrefix,
		Message: contextEntryMessage(ContextEntry{
			Kind:      ContextKindWorkingSet,
			Placement: ContextPlacementPrefix,
			Content:   sb.String(),
		}),
	}, true
}

func insertAfterDurableContextEntries(entries []HistoryEntry, extra HistoryEntry) []HistoryEntry {
	idx := 0
	for idx < len(entries) && isDurableContextEntry(entries[idx]) {
		idx++
	}
	res := make([]HistoryEntry, 0, len(entries)+1)
	res = append(res, entries[:idx]...)
	res = append(res, extra)
	res = append(res, entries[idx:]...)
	return res
}

func uniqueSorted(items []string) []string {
	if len(items) == 0 {
		return nil
	}
	out := append([]string(nil), items...)
	slices.Sort(out)
	out = slices.Compact(out)
	return out
}

func isDurableContextEntry(entry HistoryEntry) bool {
	if isPrefixContextEntry(entry) {
		return true
	}

	// Older snapshots did not persist HistoryEntry.EventType. Keep recognizing
	// the built-in context blocks so those snapshots rebuild in the same order.
	content := entry.Message.Content
	return strings.Contains(content, "<conversation_summary>") ||
		strings.Contains(content, "<working_set>")
}

func isPrefixContextEntry(entry HistoryEntry) bool {
	return entry.EventType == ContextAdded && entry.ContextPlacement == ContextPlacementPrefix
}

func subtract(items, remove []string) []string {
	if len(items) == 0 {
		return nil
	}
	if len(remove) == 0 {
		return items
	}
	removeSet := make(map[string]struct{}, len(remove))
	for _, item := range remove {
		removeSet[item] = struct{}{}
	}
	out := make([]string, 0, len(items))
	for _, item := range items {
		if _, ok := removeSet[item]; ok {
			continue
		}
		out = append(out, item)
	}
	return out
}
