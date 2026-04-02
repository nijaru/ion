package app

import (
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
)

func (m Model) View() tea.View {
	if !m.App.Ready {
		return tea.NewView("loading...")
	}

	var b strings.Builder

	// Plane B — ephemeral in-flight content
	planeB := m.renderPlaneB()
	if planeB != "" {
		b.WriteString(planeB)
	}

	// Selection overlay
	if m.Picker.Session != nil {
		b.WriteString(m.renderSessionPicker())
		b.WriteString("\n")
	} else if m.Picker.Overlay != nil {
		b.WriteString(m.renderPicker())
		b.WriteString("\n")
	}

	// Keep exactly one visual blank line between committed scrollback and Plane B.
	// Once transcript rows are printed, the terminal already advanced to the next
	// line, so we don't add another empty row unless Plane B has in-view content.
	if planeB != "" || m.Picker.Session != nil || m.Picker.Overlay != nil || !m.App.PrintedTranscript {
		b.WriteString("\n")
	}

	// Progress line
	b.WriteString(m.progressLine())
	b.WriteString("\n")

	// Top separator
	b.WriteString(m.st.sep.Render(strings.Repeat("─", max(0, m.App.Width))))
	b.WriteString("\n")

	// Composer
	b.WriteString(m.Input.Composer.View())
	b.WriteString("\n")

	// Bottom separator
	b.WriteString(m.st.sep.Render(strings.Repeat("─", max(0, m.App.Width))))
	b.WriteString("\n")

	// Status line
	b.WriteString(m.statusLine())

	return tea.NewView(b.String())
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
	b.WriteString(m.st.cyan.PaddingLeft(2).Render(m.Picker.Overlay.title))
	b.WriteString("\n")
	if m.Picker.Overlay.query != "" {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("Search: " + m.Picker.Overlay.query))
		b.WriteString("\n")
	}
	b.WriteString(
		m.st.dim.PaddingLeft(2).Render(m.renderPickerHelpText()),
	)
	b.WriteString("\n")
	if len(items) == 0 {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("No matching items"))
		b.WriteString("\n")
		return b.String()
	}
	if start > 0 {
		b.WriteString(m.st.dim.PaddingLeft(2).Render("..."))
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
			b.WriteString(m.st.dim.PaddingLeft(2).Render(item.Group))
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
		b.WriteString(m.st.dim.PaddingLeft(2).Render("..."))
		b.WriteString("\n")
	}
	return b.String()
}

func (m Model) renderPickerHelpText() string {
	if m.Picker.Overlay != nil && m.Picker.Overlay.purpose == pickerPurposeModel {
		return "Type to search • Tab swap provider/model • ↑/↓ navigate • Enter select • Esc cancel"
	}
	return "Type to search • ↑/↓ navigate • Enter select • Esc cancel"
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
	return b.String()
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
	return b.String()
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
