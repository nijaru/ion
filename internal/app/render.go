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

	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.cardTopBorder(m.Picker.Overlay.title))
	b.WriteString("\n")

	if m.Picker.Overlay.query != "" {
		b.WriteString(m.cardPaddedLine(m.st.dim, "  Search: "+m.Picker.Overlay.query))
		b.WriteString("\n")
	}

	b.WriteString(m.cardPaddedLine(m.st.dim, "  "+m.renderPickerHelpText()))
	b.WriteString("\n")

	if m.Picker.Overlay.loading {
		b.WriteString(m.cardPaddedLine(m.st.dim, "  Loading models..."))
		b.WriteString("\n")
	}
	if m.Picker.Overlay.err != "" {
		b.WriteString(m.cardPaddedLine(m.st.warn, "  Error: "+m.Picker.Overlay.err))
		b.WriteString("\n")
	}

	b.WriteString(m.cardDivider())
	b.WriteString("\n")

	if len(items) == 0 {
		if !m.Picker.Overlay.loading && m.Picker.Overlay.err == "" {
			b.WriteString(m.cardPaddedLine(m.st.dim, "  No matching items"))
			b.WriteString("\n")
		}
		b.WriteString(m.cardBottomBorder())
		return b.String()
	}

	if start > 0 {
		b.WriteString(m.cardPaddedLine(m.st.dim, "  ..."))
		b.WriteString("\n")
	}

	labelWidth := 0
	for _, item := range items {
		labelWidth = max(labelWidth, lipgloss.Width(item.Label))
	}

	if pickerHasMetrics(items) {
		metricWidths := pickerMetricWidths{}
		for _, it := range items[start:end] {
			if it.Metrics == nil {
				continue
			}
			metricWidths.Context = max(metricWidths.Context, lipgloss.Width(it.Metrics.Context))
			metricWidths.Input = max(metricWidths.Input, lipgloss.Width(it.Metrics.Input))
			metricWidths.Output = max(metricWidths.Output, lipgloss.Width(it.Metrics.Output))
		}
		metricWidths.Context = max(metricWidths.Context, lipgloss.Width("Context"))
		metricWidths.Input = max(metricWidths.Input, lipgloss.Width("Input"))
		metricWidths.Output = max(metricWidths.Output, lipgloss.Width("Output"))

		b.WriteString(m.cardPaddedLine(lipgloss.NewStyle(), m.renderPickerHeader(labelWidth, metricWidths)))
		b.WriteString("\n")
	}

	hasStructuredCols := false
	for _, item := range items[start:end] {
		if item.SettingName != "" {
			hasStructuredCols = true
			break
		}
	}

	lastGroup := ""
	for i := start; i < end; i++ {
		item := items[i]
		if item.Group != "" && item.Group != lastGroup {
			b.WriteString(m.cardPaddedLine(m.st.dim.Bold(true), "  "+item.Group))
			b.WriteString("\n")
			lastGroup = item.Group
		}

		isSelected := i == m.Picker.Overlay.index
		prefix := "  "
		if isSelected {
			prefix = "› "
		}

		var line string
		if hasStructuredCols {
			line = m.renderStructuredPickerLine(prefix, item, isSelected)
		} else {
			metricWidths := pickerMetricWidths{}
			for _, it := range items[start:end] {
				if it.Metrics == nil {
					continue
				}
				metricWidths.Context = max(metricWidths.Context, lipgloss.Width(it.Metrics.Context))
				metricWidths.Input = max(metricWidths.Input, lipgloss.Width(it.Metrics.Input))
				metricWidths.Output = max(metricWidths.Output, lipgloss.Width(it.Metrics.Output))
			}
			if pickerHasMetrics(items) {
				metricWidths.Context = max(metricWidths.Context, lipgloss.Width("Context"))
				metricWidths.Input = max(metricWidths.Input, lipgloss.Width("Input"))
				metricWidths.Output = max(metricWidths.Output, lipgloss.Width("Output"))
			}
			line = m.renderDefaultPickerLine(prefix, item, labelWidth, metricWidths, isSelected)
		}

		b.WriteString(m.cardPaddedLine(lipgloss.NewStyle(), line))
		b.WriteString("\n")
	}

	if end < len(items) {
		b.WriteString(m.cardPaddedLine(m.st.dim, "  ..."))
		b.WriteString("\n")
	}

	b.WriteString(m.cardBottomBorder())
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
	b.WriteString(m.cardTopBorder(title))
	b.WriteString("\n")
	if prompt.kind == setupPromptEndpoint {
		b.WriteString(m.cardPaddedLine(m.st.dim, "  Example: http://127.0.0.1:11434/v1"))
		b.WriteString("\n")
	}
	if prompt.err != "" {
		b.WriteString(m.cardPaddedLine(m.st.warn, "  Error: "+prompt.err))
		b.WriteString("\n")
	}
	if prompt.saving {
		b.WriteString(m.cardPaddedLine(m.st.dim, "  Saving..."))
		b.WriteString("\n")
	}
	b.WriteString(m.cardPaddedLine(lipgloss.NewStyle(), "  > "+value))
	b.WriteString("\n")
	b.WriteString(m.cardDivider())
	b.WriteString("\n")
	b.WriteString(m.cardPaddedLine(m.st.dim, "  "+help))
	b.WriteString("\n")
	b.WriteString(m.cardBottomBorder())
	return b.String()
}

func (m Model) cardTopBorder(title string) string {
	width := m.shellWidth()
	if width <= 6 {
		return m.st.dim.Render("┌" + strings.Repeat("─", max(0, width-2)) + "┐")
	}
	titleLen := lipgloss.Width(title)
	prefix := "┌─ "
	suffix := " ─"
	totalFixed := lipgloss.Width(prefix) + lipgloss.Width(suffix) + 2
	if titleLen+totalFixed >= width {
		return m.st.dim.Render("┌" + strings.Repeat("─", width-2) + "┐")
	}
	remaining := width - totalFixed - titleLen
	border := prefix + title + suffix + strings.Repeat("─", remaining) + "┐"
	return m.st.dim.Render(border)
}

func (m Model) cardBottomBorder() string {
	width := m.shellWidth()
	if width <= 2 {
		return m.st.dim.Render("└" + strings.Repeat("─", max(0, width-2)) + "┘")
	}
	return m.st.dim.Render("└" + strings.Repeat("─", width-2) + "┘")
}

func (m Model) cardDivider() string {
	width := m.shellWidth()
	if width <= 2 {
		return m.st.dim.Render("├" + strings.Repeat("─", max(0, width-2)) + "┤")
	}
	return m.st.dim.Render("├" + strings.Repeat("─", width-2) + "┤")
}

func (m Model) cardPaddedLine(style lipgloss.Style, text string) string {
	width := m.shellWidth()
	if width <= 4 {
		return m.st.dim.Render("│ ") + m.st.dim.Render(" │")
	}
	innerWidth := width - 4
	fitted := fitLine(text, innerWidth)
	plainText := ansi.Strip(fitted)
	textWidth := ansi.StringWidth(plainText)
	var pad string
	if textWidth < innerWidth {
		pad = strings.Repeat(" ", innerWidth-textWidth)
	}
	return m.st.dim.Render("│ ") + style.Render(fitted) + pad + m.st.dim.Render(" │")
}

func (m Model) renderStructuredPickerLine(prefix string, item pickerItem, isSelected bool) string {
	nameWidth := 24
	valWidth := 14

	name := item.SettingName
	if len(name) > nameWidth {
		name = name[:nameWidth-3] + "..."
	} else {
		name = name + strings.Repeat(" ", nameWidth-len(name))
	}

	var valStr string
	if item.CurrentVal != "" {
		valStr = "[ " + item.CurrentVal + " ]"
	} else {
		valStr = "[   ]"
	}

	if len(valStr) > valWidth {
		valStr = valStr[:valWidth]
	} else {
		innerLen := valWidth - 4
		valInner := item.CurrentVal
		if len(valInner) > innerLen {
			valInner = valInner[:innerLen]
		}
		padRight := innerLen - len(valInner)
		valStr = "[ " + valInner + strings.Repeat(" ", padRight) + " ]"
	}

	stylePrefix := m.st.dim
	styleName := lipgloss.NewStyle()
	styleVal := m.st.cyan
	styleDesc := m.st.dim

	if isSelected {
		stylePrefix = m.st.cyan
		styleName = m.st.cyan.Bold(true)
		if item.CurrentVal == "on" || item.CurrentVal == "active" {
			styleVal = lipgloss.NewStyle().Foreground(lipgloss.Color("2")).Bold(true)
		} else if item.CurrentVal == "off" {
			styleVal = lipgloss.NewStyle().Foreground(lipgloss.Color("9")).Bold(true)
		} else {
			styleVal = m.st.cyan.Bold(true)
		}
	} else {
		if item.CurrentVal == "on" || item.CurrentVal == "active" {
			styleVal = lipgloss.NewStyle().Foreground(lipgloss.Color("10"))
		} else if item.CurrentVal == "off" {
			styleVal = lipgloss.NewStyle().Foreground(lipgloss.Color("1"))
		} else {
			styleVal = m.st.cyan
		}
	}

	var b strings.Builder
	b.WriteString("  ")
	b.WriteString(stylePrefix.Render(prefix))
	b.WriteString(styleName.Render(name))
	b.WriteString("  ")
	b.WriteString(styleVal.Render(valStr))
	if item.Desc != "" {
		b.WriteString("  ")
		b.WriteString(styleDesc.Render(item.Desc))
	}
	return b.String()
}

func (m Model) renderDefaultPickerLine(
	prefix string,
	item pickerItem,
	labelWidth int,
	metricWidths pickerMetricWidths,
	isSelected bool,
) string {
	var b strings.Builder
	b.WriteString("  ")

	stylePrefix := m.st.dim
	styleLabel := lipgloss.NewStyle()

	if isSelected {
		stylePrefix = m.st.cyan
		styleLabel = m.st.cyan.Bold(true)
	}

	b.WriteString(stylePrefix.Render(prefix))
	label := item.Label + strings.Repeat(
		" ",
		max(0, labelWidth-lipgloss.Width(item.Label)),
	)
	b.WriteString(styleLabel.Render(label))

	if item.Metrics != nil {
		detail := m.renderPickerMetrics(*item.Metrics, metricWidths, m.st.dim)
		if detail != "" {
			b.WriteString("    ")
			b.WriteString(styleLabel.Render(detail))
		}
	} else if item.Detail != "" {
		b.WriteString("    ")
		b.WriteString(m.renderPickerDetail(item.Detail, item.Tone))
	}
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
	if m.Picker.Overlay != nil && m.Picker.Overlay.purpose == pickerPurposeSettings {
		return "Type to search • ↑/↓ move • Enter: change • Esc: close"
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
