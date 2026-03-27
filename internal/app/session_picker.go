package app

import (
	"context"
	"fmt"
	"path/filepath"
	"strings"
	"time"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/storage"
)

func (m *Model) openSessionPicker() tea.Cmd {
	m.picker = nil
	if m.store == nil {
		m.sessionPicker = &sessionPickerState{err: "session store not available"}
		return nil
	}

	sessions, err := m.store.ListSessions(context.Background(), m.workdir)
	if err != nil {
		m.sessionPicker = &sessionPickerState{err: fmt.Sprintf("failed to list sessions: %v", err)}
		return nil
	}

	items := make([]sessionPickerItem, 0, len(sessions))
	for _, info := range sessions {
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
	m.sessionPicker = state
	return nil
}

func (m Model) handleSessionPickerKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.sessionPicker == nil {
		return m, nil
	}

	switch msg.String() {
	case "esc", "ctrl+c":
		m.sessionPicker = nil
		return m, nil
	case "backspace":
		if len(m.sessionPicker.query) > 0 {
			_, size := utf8.DecodeLastRuneInString(m.sessionPicker.query)
			m.sessionPicker.query = m.sessionPicker.query[:len(m.sessionPicker.query)-size]
			m.refreshSessionPickerFilter()
		}
		return m, nil
	case "up":
		if m.sessionPicker.index > 0 {
			m.sessionPicker.index--
		}
		return m, nil
	case "down":
		if m.sessionPicker.index < len(m.sessionPicker.filtered)-1 {
			m.sessionPicker.index++
		}
		return m, nil
	case "enter":
		if len(m.sessionPicker.filtered) == 0 {
			return m, nil
		}
		selected := m.sessionPicker.filtered[m.sessionPicker.index]
		m.sessionPicker = nil
		return m, m.resumeStoredSessionByID(selected.info.ID)
	default:
		if msg.Text != "" {
			m.sessionPicker.query += msg.Text
			m.refreshSessionPickerFilter()
		}
		return m, nil
	}
}

func (m *Model) refreshSessionPickerFilter() {
	if m.sessionPicker == nil {
		return
	}
	query := strings.TrimSpace(m.sessionPicker.query)
	if query == "" {
		m.sessionPicker.filtered = append([]sessionPickerItem(nil), m.sessionPicker.items...)
	} else {
		filtered := make([]sessionPickerItem, 0, len(m.sessionPicker.items))
		for _, item := range m.sessionPicker.items {
			if sessionPickerMatches(query, item.info, m.workdir) {
				filtered = append(filtered, item)
			}
		}
		m.sessionPicker.filtered = filtered
	}
	if len(m.sessionPicker.filtered) == 0 {
		m.sessionPicker.index = 0
		return
	}
	if m.sessionPicker.index >= len(m.sessionPicker.filtered) {
		m.sessionPicker.index = len(m.sessionPicker.filtered) - 1
	}
}

func sessionPickerMatches(query string, info storage.SessionInfo, cwd string) bool {
	candidate := strings.ToLower(strings.Join([]string{
		info.ID,
		info.LastPreview,
		cwd,
	}, " "))
	q := strings.ToLower(strings.TrimSpace(query))
	if q == "" {
		return true
	}
	if strings.Contains(candidate, q) {
		return true
	}
	idx := 0
	for _, r := range q {
		next := strings.IndexRune(candidate[idx:], r)
		if next < 0 {
			return false
		}
		idx += next + utf8.RuneLen(r)
	}
	return true
}

func (m Model) renderSessionPicker() string {
	if m.sessionPicker == nil {
		return ""
	}

	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.st.cyan.PaddingLeft(2).Render("Resume a session"))
	b.WriteString("\n")
	if m.workdir != "" {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("workspace: " + filepath.Base(m.workdir)))
		b.WriteString("\n")
	}
	b.WriteString(m.st.dim.PaddingLeft(2).Render("filter: " + m.sessionPicker.query))
	b.WriteString("\n")
	if m.sessionPicker.err != "" {
		b.WriteString(m.st.warn.PaddingLeft(2).Render(m.sessionPicker.err))
		b.WriteString("\n")
	}
	if len(m.sessionPicker.filtered) == 0 {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("No matching sessions"))
		b.WriteString("\n")
		return b.String()
	}

	const maxVisible = 8
	start := 0
	if len(m.sessionPicker.filtered) > maxVisible {
		start = m.sessionPicker.index - maxVisible/2
		if start < 0 {
			start = 0
		}
		if end := start + maxVisible; end > len(m.sessionPicker.filtered) {
			start = len(m.sessionPicker.filtered) - maxVisible
		}
	}
	end := start + maxVisible
	if end > len(m.sessionPicker.filtered) {
		end = len(m.sessionPicker.filtered)
	}

	if start > 0 {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("..."))
		b.WriteString("\n")
	}
	for i := start; i < end; i++ {
		item := m.sessionPicker.filtered[i]
		line, detail := sessionPickerLine(m.workdir, item.info)
		if detail != "" {
			line += " · " + detail
		}
		if i == m.sessionPicker.index {
			b.WriteString(m.st.cyan.PaddingLeft(2).Render("› " + line))
		} else {
			b.WriteString(m.st.dim.PaddingLeft(2).Render("  " + line))
		}
		b.WriteString("\n")
	}
	if end < len(m.sessionPicker.filtered) {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("..."))
		b.WriteString("\n")
	}
	b.WriteString(m.st.dim.PaddingLeft(2).Render("type to filter · enter select · esc cancel"))
	b.WriteString("\n")
	return b.String()
}

func sessionPickerLine(cwd string, info storage.SessionInfo) (string, string) {
	label := strings.TrimSpace(info.LastPreview)
	if label == "" {
		label = info.ID
	}
	label = truncateRunes(label, 72)

	var detailParts []string
	if age := humanizeSessionAge(time.Since(info.UpdatedAt)); age != "" {
		detailParts = append(detailParts, age)
	}
	detail := strings.Join(detailParts, " · ")
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
