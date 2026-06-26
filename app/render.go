package app

import (
	"github.com/nijaru/ion/internal/runtime"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/config"
	"fmt"
	"time"
	"github.com/nijaru/ion/tool"
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/alecthomas/chroma/v2/formatters"
	"github.com/alecthomas/chroma/v2/lexers"
	chromastyles "github.com/alecthomas/chroma/v2/styles"
	"github.com/charmbracelet/x/ansi"
)

type styles struct {
	user     lipgloss.Style
	agent    lipgloss.Style
	system   lipgloss.Style
	tool     lipgloss.Style
	subagent lipgloss.Style
	success  lipgloss.Style
	dim      lipgloss.Style
	cyan     lipgloss.Style
	warn     lipgloss.Style
	caution  lipgloss.Style
	sep      lipgloss.Style
	added    lipgloss.Style
	removed  lipgloss.Style
	modeRead lipgloss.Style
	modeEdit lipgloss.Style
	modeYolo lipgloss.Style
}

func newStyles() styles {
	return styles{
		user:     lipgloss.NewStyle().Faint(true),
		agent:    lipgloss.NewStyle(),
		system:   lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Faint(true),
		tool:     lipgloss.NewStyle().Foreground(lipgloss.Color("10")),
		subagent: lipgloss.NewStyle().Foreground(lipgloss.Color("13")),
		success:  lipgloss.NewStyle().Foreground(lipgloss.Color("2")),
		dim:      lipgloss.NewStyle().Faint(true),
		cyan:     lipgloss.NewStyle().Foreground(lipgloss.Color("6")),
		warn:     lipgloss.NewStyle().Foreground(lipgloss.Color("1")),
		caution:  lipgloss.NewStyle().Foreground(lipgloss.Color("3")),
		sep:      lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Faint(true),
		added:    lipgloss.NewStyle().Foreground(lipgloss.Color("2")),
		removed:  lipgloss.NewStyle().Foreground(lipgloss.Color("1")),
		modeRead: lipgloss.NewStyle().Foreground(lipgloss.Color("6")).Bold(true),
		modeEdit: lipgloss.NewStyle().
			Foreground(lipgloss.Color("2")).
			Bold(true),
		modeYolo: lipgloss.NewStyle().
			Foreground(lipgloss.Color("1")).
			Bold(true),
	}
}

const composerPrompt = "› "

func (m Model) View() tea.View {
	if m.Picker.PreStartupMode {
		var v tea.View
		if !m.App.Ready || m.Picker.Session == nil {
			v = tea.NewView("loading...")
		} else {
			v = tea.NewView(m.renderSessionPicker())
		}
		v.AltScreen = true
		return v
	}

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

	// Progress line — suppress when Plane B already shows thinking content
	if m.InFlight.ReasonBuf == "" {
		if progress := m.progressLine(); progress != "" {
			b.WriteString(progress)
			b.WriteString("\n")
		}
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
	case runtime.SetupPromptAPIKey:
		title = "Enter API key for " + prompt.providerName
		value = strings.Repeat("•", len([]rune(prompt.value)))
	case runtime.SetupPromptEndpoint:
		title = "OpenAI-compatible endpoint"
		help = "Enter save • Esc cancel"
	default:
		title = "Provider setup"
	}

	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.cardTopBorder(title))
	b.WriteString("\n")
	if prompt.kind == runtime.SetupPromptEndpoint {
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
		return "Type to search • ↑/↓ move • Enter: select • Tab: autocomplete • Ctrl+L: cycle preset • Esc: cancel"
	}
	if m.Picker.Overlay != nil && m.Picker.Overlay.purpose == pickerPurposeProviderSetup {
		return "Type to search • ↑/↓ move • Enter: login • Esc: back to models"
	}
	if m.Picker.Overlay != nil && m.Picker.Overlay.purpose == pickerPurposeCommand {
		return "Type to search • ↑/↓ move • Enter: insert • Esc: cancel"
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

// highlightSyntax applies syntax highlighting to code using chroma.
// Returns the highlighted code as a string with ANSI escape codes.
// Falls back to plain text if the language is not recognized or highlighting fails.
func highlightSyntax(code, language string) string {
	// Get lexer for the language
	lexer := lexers.Get(language)
	if lexer == nil {
		lexer = lexers.Analyse(code)
	}
	if lexer == nil {
		lexer = lexers.Fallback
	}

	// Use catppuccin-mocha (dark theme)
	style := chromastyles.Get("catppuccin-mocha")
	if style == nil {
		style = chromastyles.Fallback
	}

	// Use terminal formatter (ANSI escape codes)
	formatter := formatters.Get("terminal")
	if formatter == nil {
		formatter = formatters.Fallback
	}

	// Tokenize and format
	iterator, err := lexer.Tokenise(nil, code)
	if err != nil {
		return code
	}

	var buf strings.Builder
	err = formatter.Format(&buf, style, iterator)
	if err != nil {
		return code
	}

	return buf.String()
}

// highlightCodeBlock applies syntax highlighting to a code block,
// preserving indentation. Each line is highlighted individually to
// maintain consistent styling across the block.
func highlightCodeBlock(code, language string, indent string) []string {
	text := strings.TrimRight(code, "\n")
	if strings.TrimSpace(text) == "" {
		return nil
	}

	// Try to highlight the entire block at once for better context
	highlighted := highlightSyntax(text, language)
	lines := strings.Split(highlighted, "\n")

	out := make([]string, 0, len(lines))
	for _, line := range lines {
		out = append(out, indent+line)
	}
	return out
}

type progressReducer struct {
	progress *ProgressState
}

func (m *Model) progressReducer() progressReducer {
	return progressReducer{progress: &m.Progress}
}

func (r progressReducer) beginLocalStatus(status string) {
	r.setLocalStatus(status)
}

func (r progressReducer) clearLocalBusyStatus() {
	if r.progress.LocalStatus != "" {
		r.setLocalStatus("")
	}
	if IsLocalBusyStatus(r.progress.Status) {
		r.setStatus("")
	}
}

func (r progressReducer) beginCompaction() {
	r.progress.Compacting = true
	r.setStatus("Compacting context...")
}

func (r progressReducer) completeCompaction() {
	r.progress.Compacting = false
	r.progress.ContextTokens = 0
	r.setStatus("Ready")
	r.clearError()
}

func (r progressReducer) clearError() {
	if r.progress.Mode == runtime.StateError {
		r.progress.Mode = runtime.StateReady
	}
	r.progress.LastError = ""
}

func (r progressReducer) setReasoningEffort(value string) {
	r.progress.ReasoningEffort = value
}

func (r progressReducer) applyRuntimeSnapshot(snapshot Snapshot) {
	r.setReasoningEffort(snapshot.Reasoning)
	if snapshot.Status != "" {
		r.setStatus(snapshot.Status)
	}
}

func (r progressReducer) markRuntimeReady() {
	r.progress.Mode = runtime.StateReady
}

func (r progressReducer) resetSessionUsage() {
	r.progress.TokensSent = 0
	r.progress.TokensReceived = 0
	r.progress.ContextTokens = 0
	r.progress.TotalCost = 0
}

func (r progressReducer) applySessionUsage(input, output int, cost float64) {
	r.progress.TokensSent = input
	r.progress.TokensReceived = output
	r.progress.TotalCost = cost
}

func (r progressReducer) setStatus(status string) {
	r.progress.Status = status
	if status == "" {
		r.progress.StatusUpdatedAt = time.Time{}
		return
	}
	r.progress.StatusUpdatedAt = time.Now()
}

func (r progressReducer) setLocalStatus(status string) {
	r.progress.LocalStatus = status
	if status == "" {
		r.progress.LocalStatusAt = time.Time{}
		return
	}
	r.progress.LocalStatusAt = time.Now()
}

func (m Model) renderQueuedTurns() string {
	count := m.queuedInputCount()
	if count == 0 {
		return ""
	}
	kind, text := m.firstQueuedInput()
	preview := compactQueuedText(text)
	label := fmt.Sprintf("• %s (Alt+Up edit): %s", kind, preview)
	if extra := count - 1; extra > 0 {
		label += fmt.Sprintf(" • +%d more", extra)
	}
	return m.st.dim.Render(fitLine(label, m.shellWidth()))
}

func (m Model) queuedInputCount() int {
	return len(m.InFlight.QueuedSteering) + len(m.InFlight.QueuedTurns)
}

func (m Model) firstQueuedInput() (string, string) {
	if len(m.InFlight.QueuedSteering) > 0 {
		return "Steering", m.InFlight.QueuedSteering[0]
	}
	if len(m.InFlight.QueuedTurns) > 0 {
		return "Queued", m.InFlight.QueuedTurns[0]
	}
	return "Queued", ""
}

func compactQueuedText(text string) string {
	return strings.Join(strings.Fields(strings.TrimSpace(text)), " ")
}

// progressLine renders the single-line progress indicator between Plane B and the composer.
func (m Model) progressLine() string {
	var line string
	idleReady := false
	if m.Progress.Compacting {
		line = m.Input.Spinner.View() + " Compacting context..."
		if n := m.queuedInputCount(); n > 0 {
			line += m.st.dim.Render(fmt.Sprintf(" • %d queued", n))
		}
		return fitLine(strings.TrimRight(line, " "), m.shellWidth())
	}
	switch m.Progress.Mode {
	case runtime.StateIonizing, runtime.StateStreaming, runtime.StateWorking:
		status := retryCountdownStatus(
			m.Progress.Status,
			m.Progress.StatusUpdatedAt,
			time.Now(),
		)
		if isIdleStatus(status) || isConfigurationStatus(status) {
			switch m.Progress.Mode {
			case runtime.StateIonizing:
				status = "Ionizing..."
			case runtime.StateStreaming:
				status = "Streaming..."
			case runtime.StateWorking:
				if len(m.InFlight.Subagents) > 0 {
					for _, k := range sortedKeys(m.InFlight.Subagents) {
						status = "Waiting for " + m.InFlight.Subagents[k].Name + "..."
						break
					}
				} else {
					status = "Working..."
				}
			}
		}
		line = m.Input.Spinner.View() + " " + status
		if stats := m.runningProgressParts(); len(stats) > 0 {
			line += m.renderProgressStats(stats)
		}
	case runtime.StateComplete:
		line = m.st.success.Render("✓") + " Complete"
		if stats := m.completedProgressParts(); len(stats) > 0 {
			line += m.renderProgressStats(stats)
		}
	case runtime.StateCancelled:
		line = m.st.warn.Render("⚠ Canceled")
		if reason := strings.TrimSpace(m.Progress.BudgetStopReason); reason != "" {
			line += " • " + reason
		}
	case runtime.StateBlocked:
		line = m.st.warn.Render("⚠ Subagent blocked")
	case runtime.StateError:
		if m.suppressTerminalErrorProgress() {
			return ""
		}
		line = m.st.warn.Render("× Error")
	default:
		if status := strings.TrimSpace(m.configurationStatus()); status != "" {
			line = m.st.warn.Render("• " + status)
		} else if status := strings.TrimSpace(m.Progress.LocalStatus); status != "" {
			line = m.st.dim.Render("• " + status)
		} else if status := strings.TrimSpace(m.Progress.Status); !isIdleStatus(status) && !isConfigurationStatus(status) {
			line = m.st.dim.Render("• " + status)
		} else {
			idleReady = true
			line = m.st.dim.Render("• Ready")
		}
	}
	if n := m.queuedInputCount(); n > 0 {
		line += m.st.dim.Render(fmt.Sprintf(" • %d queued", n))
	}
	if idleReady && m.suppressIdleReadyProgress() {
		return ""
	}
	return fitLine(strings.TrimRight(line, " "), m.shellWidth())
}

func (m Model) suppressIdleReadyProgress() bool {
	return m.App.PrintedTranscript && m.queuedInputCount() == 0
}

func (m Model) suppressTerminalErrorProgress() bool {
	return m.App.PrintedTranscript && m.queuedInputCount() == 0
}

func (m Model) renderProgressStats(parts []string) string {
	if len(parts) == 0 {
		return ""
	}
	var b strings.Builder
	for _, part := range parts {
		b.WriteString(m.st.dim.Render(" • "))
		b.WriteString(m.st.dim.Render(part))
	}
	return b.String()
}

func retryCountdownStatus(status string, updatedAt, now time.Time) string {
	if updatedAt.IsZero() || now.IsZero() {
		return status
	}
	prefix, rest, ok := strings.Cut(status, "Retrying in ")
	if !ok {
		return status
	}
	delayText, suffix, ok := strings.Cut(rest, "...")
	if !ok {
		return status
	}
	delay, err := time.ParseDuration(strings.TrimSpace(delayText))
	if err != nil {
		return status
	}
	remaining := updatedAt.Add(delay).Sub(now)
	if remaining <= 0 {
		return prefix + "Retrying now..." + suffix
	}
	return prefix + "Retrying in " + roundUpSecond(remaining).String() + "..." + suffix
}

func roundUpSecond(d time.Duration) time.Duration {
	if d <= 0 {
		return 0
	}
	return ((d + time.Second - 1) / time.Second) * time.Second
}

func (m Model) renderToolLabel(label string, isError bool) string {
	if isError {
		return m.st.warn.Render("✗") + " " + label
	}
	return m.st.tool.Render("•") + " " + label
}

func (m Model) toolOutputHidden(e session.Entry) bool {
	if session.EntryIsError(e) {
		return false
	}
	switch {
	case isReadLikeTool(session.EntryTitle(e)):
		return toolReadOutput(m.Model.Config) == "hidden"
	case isWriteTool(session.EntryTitle(e)):
		return toolWriteOutput(m.Model.Config) == "hidden"
	case isBashLikeTool(session.EntryTitle(e)):
		return toolBashOutput(m.Model.Config) == "hidden"
	default:
		return false
	}
}

func (m Model) shouldSummarizeToolOutput(e session.Entry) bool {
	if session.EntryRole(e) != session.RoleTool || session.EntryIsError(e) {
		return false
	}
	if isReadLikeTool(session.EntryTitle(e)) {
		return toolReadOutput(m.Model.Config) == "summary"
	}
	if isWriteTool(session.EntryTitle(e)) {
		return toolWriteOutput(m.Model.Config) == "summary"
	}
	if isBashLikeTool(session.EntryTitle(e)) {
		return toolBashOutput(m.Model.Config) == "summary"
	}
	if m.Model.Config != nil && m.Model.Config.ToolVerbosity == "full" {
		return false
	}
	return isReadLikeTool(session.EntryTitle(e))
}

func (m Model) shouldRenderWriteDiff(e session.Entry) bool {
	return isWriteTool(session.EntryTitle(e)) && toolWriteOutput(m.Model.Config) == "diff"
}

func toolReadOutput(cfg *config.Config) string {
	if cfg != nil {
		if output := config.NormalizeReadOutput(cfg.ReadOutput); output != "" {
			return output
		}
		switch cfg.ToolVerbosity {
		case "full":
			return "full"
		case "hidden":
			return "hidden"
		case "collapsed":
			return "summary"
		}
	}
	return "summary"
}

func toolWriteOutput(cfg *config.Config) string {
	if cfg != nil {
		if output := config.NormalizeWriteOutput(cfg.WriteOutput); output != "" {
			return output
		}
		switch cfg.ToolVerbosity {
		case "hidden":
			return "hidden"
		case "collapsed":
			return "summary"
		}
	}
	return "summary"
}

func toolBashOutput(cfg *config.Config) string {
	if cfg != nil {
		if output := config.NormalizeBashOutput(cfg.BashOutput); output != "" {
			return output
		}
		switch cfg.ToolVerbosity {
		case "full":
			return "full"
		case "collapsed":
			return "summary"
		}
	}
	return "hidden"
}

func isReadLikeTool(title string) bool {
	switch toolTitleVerb(title) {
	case "list", "ls", "read", "find", "glob", "search", "grep":
		return true
	default:
		return false
	}
}

func isBashLikeTool(title string) bool {
	switch toolTitleVerb(title) {
	case "bash":
		return true
	default:
		return false
	}
}

func toolOutputSummary(e session.Entry) string {
	trimmed := strings.TrimSpace(session.EntryContent(e))
	if trimmed == "" {
		return ""
	}
	lines := strings.Split(strings.TrimRight(session.EntryContent(e), "\n"), "\n")
	n := len(lines)
	switch toolTitleVerb(session.EntryTitle(e)) {
	case "list", "ls", "find", "glob":
		if n == 1 {
			return "1 entry"
		}
		return fmt.Sprintf("%d entries", n)
	case "grep", "search":
		if strings.TrimSuffix(strings.TrimSpace(session.EntryContent(e)), ".") == "No matches found" {
			return "0 matches"
		}
		if n == 1 {
			return "1 match"
		}
		return fmt.Sprintf("%d matches", n)
	default:
		if n == 1 {
			return "1 line"
		}
		return fmt.Sprintf("%d lines", n)
	}
}

// renderDiff colorizes diff-format output.
// Uses plain output if the content doesn't look like a unified diff.
func (m Model) renderDiff(content string) string {
	lines := strings.Split(content, "\n")
	hasDiffMarkers := false
	for _, l := range lines {
		if strings.HasPrefix(l, "--- ") || strings.HasPrefix(l, "+++ ") ||
			strings.HasPrefix(l, "@@ ") {
			hasDiffMarkers = true
			break
		}
	}
	if !hasDiffMarkers {
		return content
	}

	var b strings.Builder
	for _, l := range lines {
		switch {
		case strings.HasPrefix(l, "+") && !strings.HasPrefix(l, "+++"):
			b.WriteString(m.st.added.Render(l))
		case strings.HasPrefix(l, "-") && !strings.HasPrefix(l, "---"):
			b.WriteString(m.st.removed.Render(l))
		case strings.HasPrefix(l, "@@ "):
			b.WriteString(m.st.cyan.Render(l))
		default:
			b.WriteString(m.st.dim.Render(l))
		}
		b.WriteString("\n")
	}
	return b.String()
}

// isWriteTool returns true if the tool title looks like a write/edit operation.
func isWriteTool(title string) bool {
	lower := toolTitleVerb(title)
	for _, prefix := range []string{"write", "edit", "create"} {
		if lower == prefix {
			return true
		}
	}
	return false
}

func toolTitleVerb(title string) string {
	title = strings.TrimSpace(strings.ToLower(title))
	if title == "" {
		return ""
	}
	if idx := strings.IndexAny(title, " ("); idx >= 0 {
		return strings.TrimSpace(title[:idx])
	}
	return title
}

func (m Model) normalizeToolTitle(title string) string {
	return tool.NormalizeTitle(title, m.toolTitleOptions())
}

// FormatToolTitle attempts to extract the most important argument from a tool call's
// raw JSON string to create a more readable title.
func FormatToolTitle(name, args string) string {
	return tool.Title(name, args, tool.Options{})
}

func (m Model) formatToolTitle(name, args string) string {
	return tool.Title(name, args, tool.Options{Workdir: m.App.Workdir})
}

func (m Model) toolTitleOptions() tool.Options {
	width := 0
	if m.shellWidth() > 0 {
		width = max(0, m.shellWidth()-2)
	}
	return tool.Options{
		Workdir: m.App.Workdir,
		Width:   width,
	}
}
