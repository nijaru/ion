package app

import (
	"context"
	"fmt"
	"path/filepath"
	"slices"
	"strings"
	"time"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/storage"
)

func (m Model) openSessionPicker() (Model, tea.Cmd) {
	m.Picker.Overlay = nil
	if m.Model.Store == nil {
		m.Picker.Session = &sessionPickerState{err: "session store not available"}
		return m, nil
	}

	sessions, err := m.Model.Store.ListSessions(context.Background(), m.App.Workdir)
	if err != nil {
		m.Picker.Session = &sessionPickerState{err: fmt.Sprintf("failed to list sessions: %v", err)}
		return m, nil
	}

	items := make([]sessionPickerItem, 0, len(sessions))
	for _, info := range sessions {
		if !storage.IsConversationSessionInfo(info) {
			continue
		}
		items = append(items, sessionPickerItem{info: info})
	}

	state := &sessionPickerState{
		items:    items,
		filtered: append([]sessionPickerItem(nil), items...),
		index:    0,
	}
	if len(items) == 0 {
		state.err = "no recent sessions in this workspace"
	}
	m.Picker.Session = state
	return m, nil
}

func (m Model) handleSessionPickerKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.Picker.Session == nil {
		return m, nil
	}

	switch msg.String() {
	case "esc", "ctrl+c", "ctrl+d":
		m.Picker.Session = nil
		return m, nil
	case "backspace":
		if len(m.Picker.Session.query) > 0 {
			_, size := utf8.DecodeLastRuneInString(m.Picker.Session.query)
			m.Picker.Session.query = m.Picker.Session.query[:len(m.Picker.Session.query)-size]
			m.refreshSessionPickerFilter()
		}
		return m, nil
	case "up":
		if m.Picker.Session.index > 0 {
			m.Picker.Session.index--
		}
		return m, nil
	case "down":
		if m.Picker.Session.index < len(m.Picker.Session.filtered)-1 {
			m.Picker.Session.index++
		}
		return m, nil
	case "enter":
		if len(m.Picker.Session.filtered) == 0 {
			return m, nil
		}
		selected := m.Picker.Session.filtered[m.Picker.Session.index]
		m.Picker.Session = nil
		return m, m.resumeStoredSessionByID(selected.info.ID)
	default:
		if msg.Text != "" {
			m.Picker.Session.query += msg.Text
			m.refreshSessionPickerFilter()
		}
		return m, nil
	}
}

func (m Model) refreshSessionPickerFilter() {
	if m.Picker.Session == nil {
		return
	}
	query := strings.TrimSpace(m.Picker.Session.query)
	if query == "" {
		m.Picker.Session.filtered = append([]sessionPickerItem(nil), m.Picker.Session.items...)
	} else {
		m.Picker.Session.filtered = rankedSessionPickerItems(m.Picker.Session.items, query, m.App.Workdir)
	}
	if len(m.Picker.Session.filtered) == 0 {
		m.Picker.Session.index = 0
		return
	}
	if m.Picker.Session.index >= len(m.Picker.Session.filtered) {
		m.Picker.Session.index = len(m.Picker.Session.filtered) - 1
	}
}

func (m Model) renderSessionPicker() string {
	if m.Picker.Session == nil {
		return ""
	}

	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.st.cyan.PaddingLeft(2).Render("Resume a session"))
	b.WriteString("\n")
	if m.App.Workdir != "" {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("Workspace: " + filepath.Base(m.App.Workdir)))
		b.WriteString("\n")
	}
	b.WriteString(m.st.dim.PaddingLeft(2).Render("Search: " + m.Picker.Session.query))
	b.WriteString("\n")
	if m.Picker.Session.err != "" {
		b.WriteString(m.st.warn.PaddingLeft(2).Render(m.Picker.Session.err))
		b.WriteString("\n")
	}
	if len(m.Picker.Session.filtered) == 0 {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("No matching sessions"))
		b.WriteString("\n")
		return b.String()
	}

	const maxVisible = 8
	start := 0
	if len(m.Picker.Session.filtered) > maxVisible {
		start = m.Picker.Session.index - maxVisible/2
		if start < 0 {
			start = 0
		}
		if end := start + maxVisible; end > len(m.Picker.Session.filtered) {
			start = len(m.Picker.Session.filtered) - maxVisible
		}
	}
	end := start + maxVisible
	if end > len(m.Picker.Session.filtered) {
		end = len(m.Picker.Session.filtered)
	}

	if start > 0 {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("..."))
		b.WriteString("\n")
	}
	for i := start; i < end; i++ {
		item := m.Picker.Session.filtered[i]
		line, detail := sessionPickerLine(m.App.Workdir, item.info)
		if detail != "" {
			line += " • " + detail
		}
		if i == m.Picker.Session.index {
			b.WriteString(m.st.cyan.PaddingLeft(2).Render("› " + line))
		} else {
			b.WriteString(m.st.dim.PaddingLeft(2).Render("  " + line))
		}
		b.WriteString("\n")
	}
	if end < len(m.Picker.Session.filtered) {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("..."))
		b.WriteString("\n")
	}
	b.WriteString(m.st.dim.PaddingLeft(2).Render("Type to search • Enter select • Esc cancel"))
	b.WriteString("\n")
	return b.String()
}

func rankedSessionPickerItems(items []sessionPickerItem, query, cwd string) []sessionPickerItem {
	type rankedItem struct {
		item  sessionPickerItem
		score int
		index int
	}

	ranked := make([]rankedItem, 0, len(items))
	for i, item := range items {
		score, ok := pickerSearchScore(query,
			pickerSearchField{value: item.info.ID, weight: 0},
			pickerSearchField{value: item.info.Title, weight: 3},
			pickerSearchField{value: item.info.Summary, weight: 4},
			pickerSearchField{value: item.info.LastPreview, weight: 5},
			pickerSearchField{value: filepath.Base(cwd), weight: 10},
			pickerSearchField{value: cwd, weight: 12},
		)
		if !ok {
			continue
		}
		ranked = append(ranked, rankedItem{item: item, score: score, index: i})
	}

	slices.SortFunc(ranked, func(a, b rankedItem) int {
		if a.score != b.score {
			return a.score - b.score
		}
		if cmp := strings.Compare(strings.ToLower(a.item.info.Title), strings.ToLower(b.item.info.Title)); cmp != 0 {
			return cmp
		}
		if cmp := strings.Compare(strings.ToLower(a.item.info.Summary), strings.ToLower(b.item.info.Summary)); cmp != 0 {
			return cmp
		}
		if cmp := strings.Compare(strings.ToLower(a.item.info.LastPreview), strings.ToLower(b.item.info.LastPreview)); cmp != 0 {
			return cmp
		}
		return a.index - b.index
	})

	filtered := make([]sessionPickerItem, 0, len(ranked))
	for _, item := range ranked {
		filtered = append(filtered, item.item)
	}
	return filtered
}

func sessionPickerLine(cwd string, info storage.SessionInfo) (string, string) {
	label := strings.TrimSpace(info.Title)
	if label == "" {
		label = strings.TrimSpace(info.Summary)
	}
	if label == "" {
		label = strings.TrimSpace(info.LastPreview)
	}
	if label == "" {
		label = info.ID
	}
	label = truncateRunes(label, 72)

	var detailParts []string
	if summary := strings.TrimSpace(info.Summary); summary != "" && summary != label {
		detailParts = append(detailParts, truncateRunes(summary, 72))
	}
	if age := humanizeSessionAge(time.Since(info.UpdatedAt)); age != "" {
		detailParts = append(detailParts, age)
	}
	detail := strings.Join(detailParts, " • ")
	if detail == "" && cwd != "" {
		detail = filepath.Base(cwd)
	}
	return label, detail
}

func humanizeSessionAge(d time.Duration) string {
	if d < 0 {
		d = 0
	}
	switch {
	case d < time.Minute:
		return "just now"
	case d < time.Hour:
		return fmt.Sprintf("%dm ago", int(d.Minutes()))
	case d < 24*time.Hour:
		return fmt.Sprintf("%dh ago", int(d.Hours()))
	case d < 7*24*time.Hour:
		return fmt.Sprintf("%dd ago", int(d.Hours()/24))
	default:
		return d.Round(24 * time.Hour).String()
	}
}

func truncateRunes(s string, max int) string {
	if max <= 0 {
		return ""
	}
	if utf8.RuneCountInString(s) <= max {
		return s
	}
	var b strings.Builder
	b.Grow(max + 1)
	count := 0
	for _, r := range s {
		if count >= max-1 {
			break
		}
		b.WriteRune(r)
		count++
	}
	b.WriteString("…")
	return b.String()
}
