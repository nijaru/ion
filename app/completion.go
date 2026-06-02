package app

import (
	"github.com/nijaru/ion/config"
	"strings"

	tea "charm.land/bubbletea/v2"
	ionskills "github.com/nijaru/ion/internal/skills"
)

const maxComposerCompletions = 5

func (m *Model) refreshComposerCompletions() tea.Cmd {
	items, cmd := m.composerCompletionItems()
	m.inputReducer().setCompletionItems(items)
	return cmd
}

func (m *Model) composerCompletionItems() ([]completionItem, tea.Cmd) {
	text := m.Input.Composer.Value()
	if strings.TrimSpace(text) == "" {
		m.inputReducer().invalidateFileCompletionRequest()
		return nil, nil
	}
	if strings.HasPrefix(text, "//") {
		m.inputReducer().invalidateFileCompletionRequest()
		items := m.customComposerCompletionItems(text)
		return limitCompletionItems(items), nil
	}
	if items := slashComposerCompletionItems(text); len(items) > 0 {
		m.inputReducer().invalidateFileCompletionRequest()
		return limitCompletionItems(items), nil
	}
	start, token, ok := fileReferenceCompletionToken(text)
	if !ok {
		m.inputReducer().invalidateFileCompletionRequest()
		return nil, nil
	}
	requestID := m.inputReducer().beginFileCompletionRequest()
	return nil, loadFileReferenceCompletion(requestID, m.App.Workdir, text, start, token, false)
}

func slashComposerCompletionItems(text string) []completionItem {
	if !strings.HasPrefix(text, "/") || strings.HasPrefix(text, "//") || strings.ContainsAny(text, "\r\n") {
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

func fileReferenceCompletionToken(text string) (int, string, bool) {
	start := lastTokenStart(text)
	token := text[start:]
	if !strings.HasPrefix(token, "@") {
		return 0, "", false
	}
	return start, token, true
}

func fileReferenceCompletionItems(
	token string,
	matches []fileReferenceMatch,
) []completionItem {
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

func loadFileReferenceCompletion(
	requestID uint64,
	workdir, text string,
	start int,
	token string,
	apply bool,
) tea.Cmd {
	return func() tea.Msg {
		return fileReferenceCompletionMsg{
			requestID: requestID,
			text:      text,
			start:     start,
			token:     token,
			matches: matchingWorkspaceFileReferences(
				workdir,
				strings.TrimPrefix(token, "@"),
			),
			apply: apply,
		}
	}
}

func (m Model) handleFileReferenceCompletion(
	msg fileReferenceCompletionMsg,
) (Model, tea.Cmd) {
	if msg.requestID == 0 ||
		msg.requestID != m.Input.FileCompletionRequest ||
		m.Input.Composer.Value() != msg.text {
		return m, nil
	}
	if msg.apply {
		return m.applyFileReferenceCompletion(msg)
	}
	m.inputReducer().setCompletionItems(
		limitCompletionItems(fileReferenceCompletionItems(msg.token, msg.matches)),
	)
	return m, nil
}

func (m Model) applyFileReferenceCompletion(
	msg fileReferenceCompletionMsg,
) (Model, tea.Cmd) {
	switch len(msg.matches) {
	case 0:
		m.inputReducer().clearCompletion()
		return m, nil
	case 1:
		completion := msg.matches[0].reference
		if !msg.matches[0].isDir {
			completion += " "
		}
		return m, m.setComposerDraft(msg.text[:msg.start] + completion)
	}

	values := make([]string, 0, len(msg.matches))
	for _, match := range msg.matches {
		values = append(values, match.reference)
	}
	if prefix := commonPrefix(values); prefix != "" && prefix != msg.token {
		return m, m.setComposerDraft(msg.text[:msg.start] + prefix)
	}
	m.inputReducer().setCompletionItems(
		limitCompletionItems(fileReferenceCompletionItems(msg.token, msg.matches)),
	)
	return m, nil
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

func (m Model) customComposerCompletionItems(text string) []completionItem {
	if !strings.HasPrefix(text, "//") || strings.ContainsAny(text, "\r\n") {
		return nil
	}
	if strings.ContainsAny(text, " \t") {
		return nil
	}

	dir, err := config.DefaultSkillsDir()
	if err != nil {
		return nil
	}

	skillSummaries, err := ionskills.List(dir)
	if err != nil {
		return nil
	}

	var pickerItems []pickerItem
	for _, skill := range skillSummaries {
		search := pickerSearchIndex(
			"//"+skill.Name,
			skill.Name,
			skill.Description,
			"Skills",
			nil,
		)
		pickerItems = append(pickerItems, pickerItem{
			Label:  "//" + skill.Name,
			Value:  "//" + skill.Name,
			Detail: skill.Description,
			Group:  "Skills",
			Search: search,
		})
	}

	query := strings.TrimPrefix(strings.TrimSpace(text), "//")
	items := rankedPickerItems(pickerItems, query)
	if len(items) == 1 && strings.EqualFold(items[0].Value, strings.TrimSpace(text)) {
		return nil
	}
	return completionItemsFromPicker(items)
}
