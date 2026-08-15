package app

import (
	"context"
	"strings"
	"time"

	"github.com/nijaru/ion/config"

	tea "charm.land/bubbletea/v2"
	ionskills "github.com/nijaru/ion/internal/skills"
)

const (
	maxComposerCompletions       = 5
	skillCompletionDebounceDelay = 50 * time.Millisecond
)

func (m *Model) refreshComposerCompletions() tea.Cmd {
	text := m.Input.Composer.Value()
	if m.Input.completionText == text {
		return nil
	}
	m.Input.completionText = text
	items, cmd := m.composerCompletionItems()
	m.inputReducer().setCompletionItems(items)
	return cmd
}

func (m *Model) composerCompletionItems() ([]completionItem, tea.Cmd) {
	text := m.Input.Composer.Value()
	m.inputReducer().invalidateSkillCompletionRequest()
	if strings.TrimSpace(text) == "" {
		m.inputReducer().invalidateFileCompletionRequest()
		return nil, nil
	}
	if strings.HasPrefix(text, "//") {
		if strings.ContainsAny(text, " \t\r\n") {
			return nil, nil
		}
		requestID, ctx := m.inputReducer().beginSkillCompletionRequest(m.runtimeOperationContext())
		return nil, loadSkillCompletion(
			ctx,
			m.Model.EventGeneration,
			requestID,
			text,
			false,
		)
	}
	if items := slashComposerCompletionItems(text, m.App.Workdir); len(items) > 0 {
		m.inputReducer().invalidateFileCompletionRequest()
		return limitCompletionItems(items), nil
	}
	start, token, ok := fileReferenceCompletionToken(text)
	if !ok {
		m.inputReducer().invalidateFileCompletionRequest()
		return nil, nil
	}
	requestID := m.inputReducer().beginFileCompletionRequest()
	return nil, loadFileReferenceCompletion(
		requestID,
		m.Model.EventGeneration,
		m.App.Workdir,
		text,
		start,
		token,
		false,
	)
}

func slashComposerCompletionItems(text, workdir string) []completionItem {
	if !strings.HasPrefix(text, "/") || strings.HasPrefix(text, "//") || strings.ContainsAny(text, "\r\n") {
		return nil
	}
	if strings.ContainsAny(text, " \t") {
		return slashArgumentCompletionItems(text)
	}

	query := strings.TrimPrefix(strings.TrimSpace(text), "/")
	items := rankedPickerItems(slashCommandItemsWithPrompts(workdir), query)
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
			case "tool_mode":
				values = []string{"coding", "read", "all"}
			case "read":
				values = []string{"full", "summary", "hidden"}
			case "write":
				values = []string{"diff", "summary", "hidden"}
			case "bash":
				values = []string{"full", "summary", "hidden"}
			case "thinking":
				values = []string{"full", "collapsed", "hidden"}
			case "reasoning":
				values = []string{"auto", "off", "minimal", "low", "medium", "high", "xhigh", "max"}
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
	generation uint64,
	workdir, text string,
	start int,
	token string,
	apply bool,
) tea.Cmd {
	return func() tea.Msg {
		return fileReferenceCompletionMsg{
			generation: generation,
			requestID:  requestID,
			text:       text,
			start:      start,
			token:      token,
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
	if msg.generation != m.Model.EventGeneration ||
		msg.requestID == 0 ||
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

type skillCompletionMsg struct {
	generation uint64
	requestID  uint64
	text       string
	summaries  []ionskills.Summary
	apply      bool
	err        error
}

var loadSkillSummaries = ionskills.ListContext

func loadSkillCompletion(
	ctx context.Context,
	generation, requestID uint64,
	text string,
	apply bool,
) tea.Cmd {
	if ctx == nil {
		ctx = context.Background()
	}
	return func() tea.Msg {
		timer := time.NewTimer(skillCompletionDebounceDelay)
		defer timer.Stop()
		select {
		case <-ctx.Done():
			return skillCompletionMsg{
				generation: generation,
				requestID:  requestID,
				text:       text,
				apply:      apply,
				err:        ctx.Err(),
			}
		case <-timer.C:
		}
		if err := ctx.Err(); err != nil {
			return skillCompletionMsg{
				generation: generation,
				requestID:  requestID,
				text:       text,
				apply:      apply,
				err:        err,
			}
		}
		dir, err := config.DefaultSkillsDir()
		if err == nil {
			var summaries []ionskills.Summary
			summaries, err = loadSkillSummaries(ctx, dir)
			if err == nil {
				if ctxErr := ctx.Err(); ctxErr != nil {
					err = ctxErr
				} else {
					return skillCompletionMsg{
						generation: generation,
						requestID:  requestID,
						text:       text,
						summaries:  summaries,
						apply:      apply,
					}
				}
			}
		}
		return skillCompletionMsg{
			generation: generation,
			requestID:  requestID,
			text:       text,
			apply:      apply,
			err:        err,
		}
	}
}

func (m Model) handleSkillCompletion(msg skillCompletionMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration ||
		msg.requestID == 0 ||
		msg.requestID != m.Input.SkillCompletionRequest ||
		m.Input.Composer.Value() != msg.text {
		return m, nil
	}
	if !m.inputReducer().finishSkillCompletionRequest(msg.requestID) {
		return m, nil
	}
	if msg.err != nil {
		return m, nil
	}
	if msg.apply {
		return m.applyCustomCommandCompletion(msg.text, msg.summaries)
	}
	m.inputReducer().setCompletionItems(
		limitCompletionItems(customComposerCompletionItems(msg.text, msg.summaries)),
	)
	return m, nil
}

func customComposerCompletionItems(text string, summaries []ionskills.Summary) []completionItem {
	if !strings.HasPrefix(text, "//") || strings.ContainsAny(text, "\r\n") {
		return nil
	}
	if strings.ContainsAny(text, " \t") {
		return nil
	}

	items := rankedPickerItems(skillPickerItems(summaries), strings.TrimPrefix(strings.TrimSpace(text), "//"))
	if len(items) == 1 && strings.EqualFold(items[0].Value, strings.TrimSpace(text)) {
		return nil
	}
	return completionItemsFromPicker(items)
}

func skillPickerItems(summaries []ionskills.Summary) []pickerItem {
	items := make([]pickerItem, 0, len(summaries))
	for _, skill := range summaries {
		items = append(items, pickerItem{
			Label:  "//" + skill.Name,
			Value:  "//" + skill.Name,
			Detail: skill.Description,
			Group:  "Skills",
			Search: pickerSearchIndex(
				"//"+skill.Name,
				skill.Name,
				skill.Description,
				"Skills",
				nil,
			),
		})
	}
	return items
}
