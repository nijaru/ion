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
	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/ion/internal/storage"
)

func (m Model) openSessionPicker() (Model, tea.Cmd) {
	if m.Model.Store == nil {
		m.pickerReducer().showSessionUnavailable()
		return m, nil
	}

	requestID := m.pickerReducer().beginSessionLoad()
	return m, loadSessionPickerItems(requestID, m.Model.Store, m.App.Workdir)
}

func loadSessionPickerItems(requestID uint64, store storage.Store, workdir string) tea.Cmd {
	return func() tea.Msg {
		sessions, err := store.ListSessions(context.Background(), workdir)
		return sessionPickerLoadedMsg{requestID: requestID, sessions: sessions, err: err}
	}
}

func (m Model) handleSessionPickerLoaded(msg sessionPickerLoadedMsg) (Model, tea.Cmd) {
	m.pickerReducer().applySessionLoad(msg.requestID, msg.sessions, msg.err)
	return m, nil
}

func (m Model) handleSessionPickerKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.Picker.Session == nil {
		return m, nil
	}

	switch msg.String() {
	case "esc", "ctrl+c", "ctrl+d":
		m.pickerReducer().closeSession()
		return m, nil
	case "backspace":
		m.pickerReducer().backspaceSessionQuery(m.App.Workdir)
		return m, nil
	case "up":
		m.pickerReducer().moveSessionSelection(-1)
		return m, nil
	case "down":
		m.pickerReducer().moveSessionSelection(1)
		return m, nil
	case "pgup", "pageup":
		m.pickerReducer().pageSessionSelection(-1)
		return m, nil
	case "pgdown", "pagedown":
		m.pickerReducer().pageSessionSelection(1)
		return m, nil
	case "enter":
		selected, ok := m.pickerReducer().selectedSession()
		if !ok {
			return m, nil
		}
		m.pickerReducer().closeSession()
		return m.resumeStoredSessionByID(selected.ID)
	default:
		if text, ok := keyTextInput(msg); ok {
			m.pickerReducer().appendSessionQuery(text, m.App.Workdir)
		}
		return m, nil
	}
}

func (m Model) handleSessionPickerPaste(msg tea.PasteMsg) (Model, tea.Cmd) {
	if m.Picker.Session == nil {
		return m, nil
	}
	content := inlinePasteText(msg.Content)
	if content == "" {
		return m, nil
	}
	m.pickerReducer().appendSessionQuery(content, m.App.Workdir)
	return m, nil
}

func (m Model) renderSessionPicker() string {
	if m.Picker.Session == nil {
		return ""
	}

	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.shellPaddedLine(m.st.cyan, "Resume a session"))
	b.WriteString("\n")
	if m.App.Workdir != "" {
		workspace := "Workspace: " + filepath.Base(m.App.Workdir)
		if total := len(m.Picker.Session.items); total > 0 {
			workspace += fmt.Sprintf(" • %d sessions", total)
		}
		b.WriteString(m.shellPaddedLine(m.st.dim, workspace))
		b.WriteString("\n")
	}
	search := m.Picker.Session.query
	if search == "" {
		search = "(type to filter)"
	}
	b.WriteString(m.shellPaddedLine(m.st.dim, "Search: "+search))
	b.WriteString("\n")
	if m.Picker.Session.err != "" {
		b.WriteString(m.shellPaddedLine(m.st.warn, m.Picker.Session.err))
		b.WriteString("\n")
	}
	if m.Picker.Session.loading {
		b.WriteString(m.shellPaddedLine(m.st.dim, "Loading sessions..."))
		b.WriteString("\n")
		return b.String()
	}
	if len(m.Picker.Session.filtered) == 0 {
		b.WriteString(m.shellPaddedLine(m.st.dim, "No matching sessions"))
		b.WriteString("\n")
		return b.String()
	}

	const maxVisible = pickerPageSize
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
		b.WriteString(m.shellPaddedLine(m.st.dim, "..."))
		b.WriteString("\n")
	}
	for i := start; i < end; i++ {
		item := m.Picker.Session.filtered[i]
		selected := i == m.Picker.Session.index
		prefix := "  "
		style := m.st.dim
		if selected {
			prefix = "› "
			style = m.st.cyan
		}
		contentWidth := max(0, m.shellWidth()-ansi.StringWidth(prefix)-2)
		content := prefix + sessionPickerRenderedLine(m.App.Workdir, item.info, contentWidth)
		b.WriteString(m.shellPaddedLine(style, content))
		b.WriteString("\n")
	}
	if end < len(m.Picker.Session.filtered) {
		b.WriteString(m.shellPaddedLine(m.st.dim, "..."))
		b.WriteString("\n")
	}
	b.WriteString(
		m.shellPaddedLine(m.st.dim, "Type to search • PgUp/PgDn page • Enter select • Esc cancel"),
	)
	b.WriteString("\n")
	return b.String()
}

func rankedSessionPickerItems(items []sessionPickerItem, query, cwd string) []sessionPickerItem {
	type rankedItem struct {
		item           sessionPickerItem
		score          int
		index          int
		titleKey       string
		summaryKey     string
		lastPreviewKey string
	}

	search := preparePickerSearchQuery(query)
	cwdBase := normalizeSearchQuery(filepath.Base(cwd))
	cwdSearch := normalizeSearchQuery(cwd)
	ranked := make([]rankedItem, 0, len(items))
	for i, item := range items {
		fields := [...]pickerSearchField{
			{value: normalizeSearchQuery(item.info.ID), weight: 0},
			{value: normalizeSearchQuery(item.info.Title), weight: 3},
			{value: normalizeSearchQuery(item.info.Summary), weight: 4},
			{value: normalizeSearchQuery(item.info.LastPreview), weight: 5},
			{value: cwdBase, weight: 10},
			{value: cwdSearch, weight: 12},
		}
		score, ok := pickerSearchScorePrepared(search, fields[:])
		if !ok {
			continue
		}
		ranked = append(ranked, rankedItem{
			item:           item,
			score:          score,
			index:          i,
			titleKey:       strings.ToLower(item.info.Title),
			summaryKey:     strings.ToLower(item.info.Summary),
			lastPreviewKey: strings.ToLower(item.info.LastPreview),
		})
	}

	slices.SortFunc(ranked, func(a, b rankedItem) int {
		if a.score != b.score {
			return a.score - b.score
		}
		if cmp := strings.Compare(a.titleKey, b.titleKey); cmp != 0 {
			return cmp
		}
		if cmp := strings.Compare(a.summaryKey, b.summaryKey); cmp != 0 {
			return cmp
		}
		if cmp := strings.Compare(a.lastPreviewKey, b.lastPreviewKey); cmp != 0 {
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
	label, preview, metadataParts := sessionPickerParts(cwd, info)
	detailParts := make([]string, 0, len(metadataParts)+1)
	if preview != "" {
		detailParts = append(detailParts, preview)
	}
	detailParts = append(detailParts, metadataParts...)
	return label, strings.Join(detailParts, " • ")
}

func sessionPickerRenderedLine(cwd string, info storage.SessionInfo, width int) string {
	label, preview, metadataParts := sessionPickerParts(cwd, info)
	metadata := fitSessionPickerMetadata(metadataParts, width)
	lead := label
	if preview != "" {
		lead += " • " + preview
	}
	if metadata == "" {
		return fitLine(lead, width)
	}

	const sep = " • "
	suffixWidth := ansi.StringWidth(sep) + ansi.StringWidth(metadata)
	if width <= suffixWidth {
		return fitLine(metadata, width)
	}
	return fitLine(lead, width-suffixWidth) + sep + metadata
}

func fitSessionPickerMetadata(parts []string, width int) string {
	if len(parts) == 0 || width <= 0 {
		return ""
	}
	joined := strings.Join(parts, " • ")
	if ansi.StringWidth(joined) <= width {
		return joined
	}
	if len(parts) == 1 {
		return fitLine(parts[0], width)
	}

	const sep = " • "
	tail := fitSessionPickerMetadata(parts[1:], width)
	tailWidth := ansi.StringWidth(sep) + ansi.StringWidth(tail)
	if tail == "" {
		return fitLine(parts[0], width)
	}
	if tailWidth >= width {
		return tail
	}
	return fitLine(parts[0], width-tailWidth) + sep + tail
}

func sessionPickerParts(cwd string, info storage.SessionInfo) (string, string, []string) {
	title := strings.TrimSpace(info.Title)
	summary := strings.TrimSpace(info.Summary)
	preview := strings.TrimSpace(info.LastPreview)

	label := title
	if label == "" {
		label = preview
	}
	if label == "" {
		label = summary
	}
	if label == "" {
		label = info.ID
	}
	labelSource := label
	label = truncateRunes(label, 64)

	detailPreview := ""
	if title != "" && preview != "" && preview != labelSource {
		detailPreview = truncateRunes(preview, 64)
	}
	var metadataParts []string
	if model := strings.TrimSpace(info.Model); model != "" {
		metadataParts = append(metadataParts, model)
	}
	if branch := strings.TrimSpace(info.Branch); branch != "" {
		metadataParts = append(metadataParts, branch)
	}
	if age := sessionAgeLabel(info.UpdatedAt); age != "" {
		metadataParts = append(metadataParts, age)
	}
	if len(metadataParts) == 0 && cwd != "" {
		metadataParts = append(metadataParts, filepath.Base(cwd))
	}
	return label, detailPreview, metadataParts
}

func sessionAgeLabel(updatedAt time.Time) string {
	if updatedAt.IsZero() {
		return ""
	}
	return humanizeSessionAge(time.Since(updatedAt))
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
		return fmt.Sprintf("%dd ago", int(d.Hours()/24))
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
