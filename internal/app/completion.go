package app

import (
	"strings"
)

const maxComposerCompletions = 5

func (m *Model) refreshComposerCompletions() {
	items := m.composerCompletionItems()
	if len(items) == 0 {
		m.Input.Completion = nil
		return
	}
	m.Input.Completion = &completionState{items: items}
}

func (m Model) composerCompletionItems() []completionItem {
	text := m.Input.Composer.Value()
	if strings.TrimSpace(text) == "" {
		return nil
	}
	if items := slashComposerCompletionItems(text); len(items) > 0 {
		return limitCompletionItems(items)
	}
	if items := m.fileReferenceCompletionItems(text); len(items) > 0 {
		return limitCompletionItems(items)
	}
	return nil
}

func slashComposerCompletionItems(text string) []completionItem {
	if !strings.HasPrefix(text, "/") || strings.ContainsAny(text, "\r\n") {
		return nil
	}
	if strings.ContainsAny(text, " \t") {
		return slashArgumentCompletionItems(text)
	}

	query := strings.TrimPrefix(strings.TrimSpace(text), "/")
	items := rankedPickerItems(slashCommandItems(), query)
	if len(items) == 1 && strings.EqualFold(items[0].Value, strings.TrimSpace(text)) {
		return nil
	}
	return completionItemsFromPicker(items)
}

func slashArgumentCompletionItems(text string) []completionItem {
	fields := strings.Fields(text)
	if len(fields) == 0 {
		return nil
	}
	if strings.HasSuffix(text, " ") || strings.HasSuffix(text, "\t") {
		return nil
	}

	var values []string
	switch fields[0] {
	case "/thinking":
		if len(fields) == 2 {
			values = thinkingCompletionValues()
		}
	case "/settings":
		if len(fields) == 2 {
			values = settingsCompletionKeys()
		} else if len(fields) == 3 {
			switch normalizeSettingsCompletionKey(fields[1]) {
			case "retry":
				values = []string{"on", "off"}
			case "tool":
				values = []string{"auto", "full", "collapsed", "hidden"}
			case "read":
				values = []string{"full", "summary", "hidden"}
			case "write":
				values = []string{"diff", "summary", "hidden"}
			case "bash":
				values = []string{"full", "summary", "hidden"}
			case "thinking":
				values = []string{"full", "collapsed", "hidden"}
			case "busy":
				values = []string{"queue", "steer"}
			}
		}
	}
	if len(values) == 0 {
		return nil
	}

	prefix := text[lastTokenStart(text):]
	matches := matchingValues(prefix, values)
	if len(matches) == 1 && strings.EqualFold(matches[0], prefix) {
		return nil
	}
	items := make([]completionItem, 0, len(matches))
	for _, match := range matches {
		items = append(items, completionItem{Label: match})
	}
	return items
}

func (m Model) fileReferenceCompletionItems(text string) []completionItem {
	start := lastTokenStart(text)
	token := text[start:]
	if !strings.HasPrefix(token, "@") {
		return nil
	}
	matches := matchingWorkspaceFileReferences(m.App.Workdir, strings.TrimPrefix(token, "@"))
	if len(matches) == 1 && strings.EqualFold(matches[0].reference, token) {
		return nil
	}
	items := make([]completionItem, 0, len(matches))
	for _, match := range matches {
		detail := ""
		if match.isDir {
			detail = "directory"
		}
		items = append(items, completionItem{Label: match.reference, Detail: detail})
	}
	return items
}

func completionItemsFromPicker(items []pickerItem) []completionItem {
	out := make([]completionItem, 0, len(items))
	for _, item := range items {
		out = append(out, completionItem{Label: item.Label, Detail: item.Detail})
	}
	return out
}

func limitCompletionItems(items []completionItem) []completionItem {
	if len(items) <= maxComposerCompletions {
		return items
	}
	return items[:maxComposerCompletions]
}
