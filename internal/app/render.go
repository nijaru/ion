package app

import (
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
)

const composerPrompt = "› "

func (m Model) View() tea.View {
	if !m.App.Ready {
		return tea.NewView("loading...")
	}

	var b strings.Builder

	// Plane B — ephemeral in-flight content
	planeB := m.renderPlaneB()
	hasShellLeadIn := false
	if planeB != "" {
		b.WriteString(planeB)
		hasShellLeadIn = true
	}

	// Selection overlay
	if m.Picker.Session != nil {
		b.WriteString(m.renderSessionPicker())
		b.WriteString("\n")
		hasShellLeadIn = true
	} else if m.Picker.Setup != nil {
		b.WriteString(m.renderSetupPrompt())
		b.WriteString("\n")
		hasShellLeadIn = true
	} else if m.Picker.Overlay != nil {
		b.WriteString(m.renderPicker())
		b.WriteString("\n")
		hasShellLeadIn = true
	}

	if hasShellLeadIn && !strings.HasSuffix(b.String(), "\n\n") {
		b.WriteString("\n")
	}

	if queued := m.renderQueuedTurns(); queued != "" {
		b.WriteString(queued)
		b.WriteString("\n\n")
	}

	b.WriteString(m.renderShell())
	return tea.NewView(b.String())
}

func (m Model) renderShell() string {
	var b strings.Builder

	// Progress line
	if progress := m.progressLine(); progress != "" {
		b.WriteString(progress)
		b.WriteString("\n")
	}

	b.WriteString(m.st.sep.Render(m.shellSeparator()))
	b.WriteString("\n")

	// Composer
	b.WriteString(m.renderComposer())
	b.WriteString("\n")
	if completions := m.renderComposerCompletions(); completions != "" {
		b.WriteString(completions)
		b.WriteString("\n")
	}

	// Bottom separator
	b.WriteString(m.st.sep.Render(m.shellSeparator()))
	b.WriteString("\n")

	// Status line
	b.WriteString(m.statusLine())

	return b.String()
}

func (m Model) renderComposer() string {
	return renderComposerView(m.Input.Composer.View(), m.shellWidth())
}

func renderComposerView(view string, width int) string {
	rows := strings.Split(view, "\n")
	continuationPrompt := strings.Repeat(" ", composerPromptWidth())
	for i := range rows {
		prompt := continuationPrompt
		if i == 0 {
			prompt = composerPrompt
		}
		rows[i] = fitLine(prompt+rows[i], width)
	}
	return strings.Join(rows, "\n")
}

func composerPromptWidth() int {
	return ansi.StringWidth(composerPrompt)
}

func (m Model) renderComposerCompletions() string {
	if m.Picker.Overlay != nil ||
		m.Picker.Session != nil ||
		m.Picker.Setup != nil ||
		m.Input.Completion == nil ||
		len(m.Input.Completion.items) == 0 {
		return ""
	}

	labelWidth := 0
	for _, item := range m.Input.Completion.items {
		labelWidth = max(labelWidth, lipgloss.Width(item.Label))
	}

	lines := make([]string, 0, len(m.Input.Completion.items))
	for _, item := range m.Input.Completion.items {
		line := item.Label
		if item.Detail != "" {
			line += strings.Repeat(" ", max(2, labelWidth-lipgloss.Width(item.Label)+2))
			line += item.Detail
		}
		lines = append(lines, m.shellPaddedLine(m.st.dim, line))
	}
	return strings.Join(lines, "\n")
}

func (m Model) shellWidth() int {
	if m.App.Width <= 1 {
		return max(0, m.App.Width)
	}
	// Inline terminal rows that exactly fill the terminal can auto-wrap into an
	// extra physical row. Keep live shell chrome one cell short so resize redraws
	// do not leave stale progress/status fragments behind.
	return m.App.Width - 1
}

func (m Model) shellSeparator() string {
	width := m.shellWidth()
	if width <= 0 {
		return ""
	}
	return strings.Repeat("─", width)
}

func (m Model) shellPaddedLine(style lipgloss.Style, text string) string {
	width := m.shellWidth()
	if width <= 0 {
		return ""
	}
	if width <= 2 {
		return fitLine(style.Render(text), width)
	}
	return style.PaddingLeft(2).Render(fitLine(text, width-2))
}

func (m Model) renderPicker() string {
	if m.Picker.Overlay == nil {
		return ""
	}
	items := pickerDisplayItems(m.Picker.Overlay)

	const maxVisible = 8
	start := 0
	if len(items) > maxVisible {
		start = m.Picker.Overlay.index - maxVisible/2
		if start < 0 {
			start = 0
		}
		if end := start + maxVisible; end > len(items) {
			start = len(items) - maxVisible
		}
	}
	end := start + maxVisible
	if end > len(items) {
		end = len(items)
	}

	metricWidths := pickerMetricWidths{}
	for _, item := range items[start:end] {
		if item.Metrics == nil {
			continue
		}
		metricWidths.Context = max(metricWidths.Context, lipgloss.Width(item.Metrics.Context))
		metricWidths.Input = max(metricWidths.Input, lipgloss.Width(item.Metrics.Input))
		metricWidths.Output = max(metricWidths.Output, lipgloss.Width(item.Metrics.Output))
	}
	if pickerHasMetrics(items) {
		metricWidths.Context = max(metricWidths.Context, lipgloss.Width("Context"))
		metricWidths.Input = max(metricWidths.Input, lipgloss.Width("Input"))
		metricWidths.Output = max(metricWidths.Output, lipgloss.Width("Output"))
	}

	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.shellPaddedLine(m.st.cyan, m.Picker.Overlay.title))
	b.WriteString("\n")
	if m.Picker.Overlay.query != "" {
		b.WriteString(m.shellPaddedLine(m.st.dim, "Search: "+m.Picker.Overlay.query))
		b.WriteString("\n")
	}
	b.WriteString(m.shellPaddedLine(m.st.dim, m.renderPickerHelpText()))
	b.WriteString("\n")
	if m.Picker.Overlay.loading {
		b.WriteString(m.shellPaddedLine(m.st.dim, "Loading models..."))
		b.WriteString("\n")
	}
	if m.Picker.Overlay.err != "" {
		b.WriteString(m.shellPaddedLine(m.st.warn, m.Picker.Overlay.err))
		b.WriteString("\n")
	}
	if len(items) == 0 {
		if !m.Picker.Overlay.loading && m.Picker.Overlay.err == "" {
			b.WriteString(m.shellPaddedLine(m.st.dim, "No matching items"))
			b.WriteString("\n")
		}
		return b.String()
	}
	if start > 0 {
		b.WriteString(m.shellPaddedLine(m.st.dim, "..."))
		b.WriteString("\n")
	}
	labelWidth := 0
	for _, item := range items {
		labelWidth = max(labelWidth, lipgloss.Width(item.Label))
	}
	if pickerHasMetrics(items) {
		b.WriteString(m.renderPickerHeader(labelWidth, metricWidths))
		b.WriteString("\n")
	}
	lastGroup := ""
	for i := start; i < end; i++ {
		item := items[i]
		if item.Group != "" && item.Group != lastGroup {
			b.WriteString("\n")
			b.WriteString(m.shellPaddedLine(m.st.dim, item.Group))
			b.WriteString("\n")
			lastGroup = item.Group
		}
		if i == m.Picker.Overlay.index {
			b.WriteString(
				m.renderPickerLine("› ", item, labelWidth, metricWidths, m.st.cyan, m.st.cyan),
			)
		} else {
			b.WriteString(m.renderPickerLine("  ", item, labelWidth, metricWidths, lipgloss.NewStyle(), m.st.dim))
		}
		b.WriteString("\n")
	}
	if end < len(items) {
		b.WriteString(m.shellPaddedLine(m.st.dim, "..."))
		b.WriteString("\n")
	}
	return b.String()
}

func (m Model) renderSetupPrompt() string {
	if m.Picker.Setup == nil {
		return ""
	}
	prompt := m.Picker.Setup
	title := ""
	help := "Enter save • Esc cancel"
	value := prompt.value
	switch prompt.kind {
	case setupPromptAPIKey:
		title = "Enter API key for " + prompt.providerName
		value = strings.Repeat("•", len([]rune(prompt.value)))
	case setupPromptEndpoint:
		title = "OpenAI-compatible endpoint"
		help = "Enter save • Esc cancel"
	default:
		title = "Provider setup"
	}

	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.shellPaddedLine(m.st.cyan, title))
	b.WriteString("\n")
	if prompt.kind == setupPromptEndpoint {
		b.WriteString(m.shellPaddedLine(m.st.dim, "Example: http://127.0.0.1:11434/v1"))
		b.WriteString("\n")
	}
	if prompt.err != "" {
		b.WriteString(m.shellPaddedLine(m.st.warn, prompt.err))
		b.WriteString("\n")
	}
	if prompt.saving {
		b.WriteString(m.shellPaddedLine(m.st.dim, "Saving..."))
		b.WriteString("\n")
	}
	b.WriteString(m.shellPaddedLine(lipgloss.NewStyle(), "> "+value))
	b.WriteString("\n")
	b.WriteString(m.shellPaddedLine(m.st.dim, help))
	b.WriteString("\n")
	return b.String()
}

func (m Model) renderPickerHelpText() string {
	if m.Picker.Overlay != nil && m.Picker.Overlay.purpose == pickerPurposeModel {
		return "Type to search • ↑/↓ move • Enter: select • Tab: providers • Ctrl+M: primary/fast • Esc: cancel"
	}
	if m.Picker.Overlay != nil && m.Picker.Overlay.purpose == pickerPurposeCommand {
		return "Type to search • ↑/↓ move • Enter: insert • Esc: cancel"
	}
	if m.Picker.Overlay != nil && m.Picker.Overlay.purpose == pickerPurposeProvider {
		return "Type to search • ↑/↓ move • Enter: select • Tab: models • Esc: cancel"
	}
	return "Type to search • ↑/↓ move • Enter: select • Esc: cancel"
}

type pickerMetricWidths struct {
	Context int
	Input   int
	Output  int
}

func (m Model) renderPickerHeader(labelWidth int, metricWidths pickerMetricWidths) string {
	var b strings.Builder
	b.WriteString(strings.Repeat(" ", 4))
	b.WriteString(
		m.st.dim.Render("Model" + strings.Repeat(" ", max(0, labelWidth-lipgloss.Width("Model")))),
	)
	detail := m.renderPickerHeaderMetrics(metricWidths)
	if detail != "" {
		b.WriteString("    ")
		b.WriteString(detail)
	}
	return fitLine(b.String(), m.shellWidth())
}

func (m Model) renderPickerLine(
	prefix string,
	item pickerItem,
	labelWidth int,
	metricWidths pickerMetricWidths,
	labelStyle, metricStyle lipgloss.Style,
) string {
	var b strings.Builder
	b.WriteString(strings.Repeat(" ", 2))
	label := prefix + item.Label + strings.Repeat(
		" ",
		max(0, labelWidth-lipgloss.Width(item.Label)),
	)
	b.WriteString(labelStyle.Render(label))
	if item.Metrics != nil {
		detail := m.renderPickerMetrics(*item.Metrics, metricWidths, metricStyle)
		if detail != "" {
			b.WriteString("    ")
			b.WriteString(detail)
		}
	} else if item.Detail != "" {
		b.WriteString("    ")
		b.WriteString(m.renderPickerDetail(item.Detail, item.Tone))
	}
	return fitLine(b.String(), m.shellWidth())
}

func (m Model) renderPickerMetrics(
	metrics pickerMetrics,
	widths pickerMetricWidths,
	style lipgloss.Style,
) string {
	var parts []string
	if widths.Context > 0 {
		parts = append(parts, m.renderPickerMetricValue(metrics.Context, widths.Context, style))
	}
	if widths.Input > 0 {
		parts = append(parts, m.renderPickerMetricValue(metrics.Input, widths.Input, style))
	}
	if widths.Output > 0 {
		parts = append(parts, m.renderPickerMetricValue(metrics.Output, widths.Output, style))
	}
	return strings.Join(parts, "  ")
}

func (m Model) renderPickerHeaderMetrics(widths pickerMetricWidths) string {
	var parts []string
	if widths.Context > 0 {
		parts = append(parts, m.renderPickerMetricHeading("Context", widths.Context))
	}
	if widths.Input > 0 {
		parts = append(parts, m.renderPickerMetricHeading("Input", widths.Input))
	}
	if widths.Output > 0 {
		parts = append(parts, m.renderPickerMetricHeading("Output", widths.Output))
	}
	return strings.Join(parts, "  ")
}

func (m Model) renderPickerMetricHeading(value string, width int) string {
	pad := strings.Repeat(" ", max(0, width-lipgloss.Width(value)))
	return m.st.dim.Render(value + pad)
}

func (m Model) renderPickerMetricValue(value string, width int, style lipgloss.Style) string {
	shown := strings.TrimSpace(value)
	if shown == "" {
		shown = "—"
	}
	pad := strings.Repeat(" ", max(0, width-lipgloss.Width(shown)))
	return style.Render(shown + pad)
}

func pickerHasMetrics(items []pickerItem) bool {
	for _, item := range items {
		if item.Metrics != nil {
			return true
		}
	}
	return false
}

func (m Model) renderPickerDetail(detail string, tone pickerTone) string {
	switch tone {
	case pickerToneWarn:
		return m.st.warn.Render(detail)
	default:
		return m.st.dim.Render(detail)
	}
}
