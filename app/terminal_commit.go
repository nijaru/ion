package app

import (
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/session"
)

const (
	printSubmitHoldThreshold = 12
	printSubmitHoldBase      = 150 * time.Millisecond
	printSubmitHoldPerLine   = 15 * time.Millisecond
	printSubmitHoldMax       = 1 * time.Second
	printFrameSettleDelay    = 16 * time.Millisecond
)

type terminalCommitController struct {
	model *Model
}

func (m *Model) terminalCommit() terminalCommitController {
	return terminalCommitController{model: m}
}

func (c terminalCommitController) MarkPrinted() {
	c.model.App.PrintedTranscript = true
}

func (c terminalCommitController) Entries(entries ...session.Entry) tea.Cmd {
	if len(entries) == 0 {
		return nil
	}
	lines := make([]string, 0, len(entries))
	c.MarkPrinted()
	lines = append(lines, c.model.RenderEntries(entries...)...)
	c.model.holdEnterForLargePrint(physicalLineCount(lines))
	return deferredTerminalCommitCmd(lines...)
}

func (c terminalCommitController) Help(content string) tea.Cmd {
	content = strings.TrimRight(content, "\n")
	if content == "" {
		return nil
	}
	lines := make([]string, 0, strings.Count(content, "\n")+1)
	c.MarkPrinted()
	for i, line := range strings.Split(content, "\n") {
		lines = append(lines, c.model.renderHelpLine(i, line))
	}
	c.model.holdEnterForLargePrint(physicalLineCount(lines))
	return deferredTerminalCommitCmd(lines...)
}

func (c terminalCommitController) Lines(lines ...string) tea.Cmd {
	if len(lines) == 0 {
		return nil
	}
	c.MarkPrinted()
	c.model.holdEnterForLargePrint(physicalLineCount(lines))
	return deferredTerminalCommitCmd(lines...)
}

func (c terminalCommitController) DeferredLines(lines ...string) tea.Cmd {
	return c.Lines(lines...)
}

func (c terminalCommitController) SwitchReplay(
	printLines []string,
	entries []session.Entry,
	notice string,
	status string,
) tea.Cmd {
	c.MarkPrinted()
	var lines []string
	if len(printLines) > 0 {
		lines = append(lines, printLines...)
	}
	if len(entries) > 0 {
		if len(lines) > 0 {
			lines = append(lines, "")
		}
		lines = append(lines, c.model.RenderEntries(entries...)...)
	}
	if strings.TrimSpace(notice) != "" {
		if len(lines) > 0 {
			lines = append(lines, "")
		}
		lines = append(lines, c.model.renderEntry(systemEntry(notice)))
	}
	if strings.TrimSpace(status) != "" {
		if len(lines) > 0 {
			lines = append(lines, "")
		}
		lines = append(lines, c.model.renderEntry(systemEntry(status)))
	}
	if len(lines) == 0 {
		return nil
	}
	c.model.holdEnterForLargePrint(physicalLineCount(lines))
	return deferredTerminalCommitCmd(lines...)
}

func (m Model) RenderEntries(entries ...session.Entry) []string {
	lines := make([]string, 0, len(entries)*2)
	for _, entry := range entries {
		if len(lines) > 0 {
			lines = append(lines, "")
		}
		lines = append(lines, m.renderEntry(entry))
	}
	return lines
}

func (m Model) handleLocalEntries(msg localEntriesMsg) (Model, tea.Cmd) {
	return m, m.terminalCommit().Entries(msg.entries...)
}

func terminalCommitFlushCmd(lines ...string) tea.Cmd {
	body := formatPrintLines(lines...)
	if body == "" {
		return nil
	}
	return tea.Printf("%s", body)
}

func deferredTerminalCommitCmd(lines ...string) tea.Cmd {
	copied := append([]string(nil), lines...)
	return tea.Tick(printFrameSettleDelay, func(time.Time) tea.Msg {
		return terminalCommitLinesMsg{lines: copied}
	})
}

func formatPrintLines(lines ...string) string {
	filtered := make([]string, 0, physicalLineCount(lines))
	for _, line := range lines {
		filtered = append(filtered, strings.Split(line, "\n")...)
	}
	for len(filtered) > 0 && filtered[len(filtered)-1] == "" {
		filtered = filtered[:len(filtered)-1]
	}
	if len(filtered) == 0 {
		return ""
	}
	for i, line := range filtered {
		if line == "" {
			filtered[i] = "\x1b[0m"
		}
	}
	filtered = append(filtered, "")
	return strings.Join(filtered, "\n")
}

func physicalLineCount(lines []string) int {
	count := 0
	for _, line := range lines {
		count += strings.Count(line, "\n") + 1
	}
	return count
}

func (m *Model) holdEnterForLargePrint(lines int) {
	if lines < printSubmitHoldThreshold {
		return
	}
	delay := printSubmitHoldBase + time.Duration(lines)*printSubmitHoldPerLine
	if delay > printSubmitHoldMax {
		delay = printSubmitHoldMax
	}
	m.inputReducer().holdEnter(delay)
}

func (m Model) printHoldActive() bool {
	return time.Now().Before(m.Input.PrintHoldUntil)
}

func (m Model) scheduleDeferredEnter() tea.Cmd {
	delay := time.Until(m.Input.PrintHoldUntil)
	if delay < 10*time.Millisecond {
		delay = 10 * time.Millisecond
	}
	return tea.Tick(delay, func(time.Time) tea.Msg {
		return deferredEnterMsg{}
	})
}
