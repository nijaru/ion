package app

import (
	"fmt"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
)

func TestLayoutClampsComposerHeight(t *testing.T) {
	model := readyModel(t)

	// Initial height should be min (1)
	model.layout()
	if got := model.Input.Composer.Height(); got != minComposerHeight {
		t.Fatalf("expected initial composer height %d, got %d", minComposerHeight, got)
	}

	// 5 lines of text
	model.Input.Composer.SetValue("1\n2\n3\n4\n5")
	model.layout()

	// Should be 5
	if got := model.Input.Composer.Height(); got != 5 {
		t.Fatalf("expected composer height 5 for 5 lines, got %d", got)
	}

	// Over the max (10)
	model.Input.Composer.SetValue(strings.Repeat("line\n", 20))
	model.layout()

	if got := model.Input.Composer.Height(); got != maxComposerHeight {
		t.Fatalf("expected composer height to clamp to %d, got %d", maxComposerHeight, got)
	}
}

func TestLayoutExpandsComposerForSoftWrappedInput(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 24, Height: 30})
	model = testModel(t, updated)
	model.Input.Composer.SetValue("write a sentence that wraps")

	model.layout()

	if got := model.Input.Composer.Height(); got < 2 {
		t.Fatalf("composer height = %d, want soft-wrapped text to expand", got)
	}
}

func TestComposerKeepsSoftWrappedPrefixVisibleWhileTyping(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 60, Height: 30})
	model = testModel(t, updated)

	input := "write a sentence that should wrap across multiple terminal rows before submit"
	for _, r := range input {
		updated, _ = model.Update(tea.KeyPressMsg{Code: r, Text: string(r)})
		model = testModel(t, updated)
	}

	view := ansi.Strip(model.View().Content)
	if !strings.Contains(view, "write a sentence") {
		t.Fatalf("composer lost soft-wrapped prefix:\n%s", view)
	}
	if !strings.Contains(view, "before submit") {
		t.Fatalf("composer lost soft-wrapped suffix:\n%s", view)
	}
}

func TestComposerSupportsHardMultilineInput(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 60, Height: 30})
	model = testModel(t, updated)

	for _, r := range "first line" {
		updated, _ = model.Update(tea.KeyPressMsg{Code: r, Text: string(r)})
		model = testModel(t, updated)
	}
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyEnter, Mod: tea.ModShift})
	model = testModel(t, updated)
	for _, r := range "second line" {
		updated, _ = model.Update(tea.KeyPressMsg{Code: r, Text: string(r)})
		model = testModel(t, updated)
	}

	if got := model.Input.Composer.Value(); got != "first line\nsecond line" {
		t.Fatalf("composer = %q, want hard multiline input", got)
	}
	view := ansi.Strip(model.View().Content)
	first := strings.Index(view, "› first line")
	second := strings.Index(view, "  second line")
	if first < 0 || second < 0 {
		t.Fatalf("composer did not render both hard lines:\n%s", view)
	}
	if first > second {
		t.Fatalf("composer rendered hard lines out of order:\n%s", view)
	}
	if strings.Contains(view, "› second line") {
		t.Fatalf("composer repeated prompt marker on continuation row:\n%s", view)
	}
	if strings.Contains(view, "first linesecond line") {
		t.Fatalf("composer collapsed hard newline into one row:\n%s", view)
	}
}

func TestComposerSupportsCtrlJMultilineInput(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 60, Height: 30})
	model = testModel(t, updated)

	for _, r := range "first line" {
		updated, _ = model.Update(tea.KeyPressMsg{Code: r, Text: string(r)})
		model = testModel(t, updated)
	}
	updated, _ = model.Update(tea.KeyPressMsg{Code: 'j', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	for _, r := range "second line" {
		updated, _ = model.Update(tea.KeyPressMsg{Code: r, Text: string(r)})
		model = testModel(t, updated)
	}

	if got := model.Input.Composer.Value(); got != "first line\nsecond line" {
		t.Fatalf("composer = %q, want ctrl+j multiline input", got)
	}
	view := ansi.Strip(model.View().Content)
	if !strings.Contains(view, "› first line") || !strings.Contains(view, "  second line") {
		t.Fatalf("composer did not render ctrl+j multiline input:\n%s", view)
	}
	if strings.Contains(view, "› second line") {
		t.Fatalf("composer repeated prompt marker on continuation row:\n%s", view)
	}
	if strings.Contains(view, "first linesecond line") {
		t.Fatalf("composer collapsed ctrl+j newline into one row:\n%s", view)
	}
}

func TestComposerPromptOnlyRendersOnFirstRow(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 60, Height: 30})
	model = testModel(t, updated)
	model.Input.Composer.SetValue(strings.Repeat("\n", 5))
	model.layout()

	rows := renderedComposerRows(t, model)
	if len(rows) < 2 {
		t.Fatalf("composer rows = %#v, want multiple rows", rows)
	}
	if !strings.HasPrefix(rows[0], "› ") {
		t.Fatalf("first composer row = %q, want prompt marker", rows[0])
	}
	for i, row := range rows[1:] {
		if strings.HasPrefix(row, "›") {
			t.Fatalf("composer row %d repeated prompt marker: %#v", i+2, rows)
		}
	}
}

func renderedComposerRows(t *testing.T, model Model) []string {
	t.Helper()
	view := ansi.Strip(model.View().Content)
	lines := strings.Split(view, "\n")
	firstSeparator := -1
	for i, line := range lines {
		if line == "" || strings.Trim(line, "─") != "" {
			continue
		}
		if firstSeparator < 0 {
			firstSeparator = i
			continue
		}
		return lines[firstSeparator+1 : i]
	}
	t.Fatalf("view did not contain shell separators:\n%s", view)
	return nil
}

func TestProgressLineFitsWidthAfterResize(t *testing.T) {
	model := readyModel(t)
	model.App.Width = 28
	model.Progress.Mode = stateError
	model.Progress.LastError = strings.Repeat(
		"connection refused while reconnecting to the backend ",
		3,
	)

	if got := lipgloss.Width(model.progressLine()); got > model.shellWidth() {
		t.Fatalf(
			"expected progress line width <= %d, got %d: %q",
			model.shellWidth(),
			got,
			model.progressLine(),
		)
	}
}

func TestViewSuppressesIdleReadyAfterPrintedTranscript(t *testing.T) {
	model := readyModel(t)
	model.App.PrintedTranscript = true
	model.Progress.Mode = stateReady

	view := ansi.Strip(model.View().Content)
	if strings.Contains(view, "• Ready") {
		t.Fatalf("view = %q, want no committed idle ready row after printed transcript", view)
	}
	if strings.HasPrefix(view, "\n") {
		t.Fatalf("view = %q, want shell without leading blank line", view)
	}
	if !strings.HasPrefix(view, "─") {
		t.Fatalf("view = %q, want shell separator first when idle progress is hidden", view)
	}
}

func TestViewKeepsTerminalProgressAfterPrintedTranscript(t *testing.T) {
	model := readyModel(t)
	model.App.PrintedTranscript = true
	model.Progress.Mode = stateComplete

	view := ansi.Strip(model.View().Content)
	if !strings.HasPrefix(view, "✓ Complete\n") {
		t.Fatalf("view = %q, want terminal progress after printed transcript", view)
	}
}

func TestViewShellRowsReserveWrapCellAfterResize(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 40, Height: 24})
	model = testModel(t, updated)
	model.Progress.Mode = stateReady

	view := ansi.Strip(model.View().Content)
	wantWidth := model.shellWidth()
	separatorCount := 0
	for i, line := range strings.Split(view, "\n") {
		if line == "" {
			continue
		}
		if got := ansi.StringWidth(line); got > wantWidth {
			t.Fatalf("line %d width = %d, want <= %d: %q\nview:\n%s", i, got, wantWidth, line, view)
		}
		if strings.Trim(line, "─") == "" {
			separatorCount++
			if got := ansi.StringWidth(line); got != wantWidth {
				t.Fatalf("separator width = %d, want %d: %q", got, wantWidth, line)
			}
		}
	}
	if separatorCount != 2 {
		t.Fatalf("separator count = %d, want 2:\n%s", separatorCount, view)
	}
	if count := strings.Count(view, "• Ready"); count != 1 {
		t.Fatalf("ready row count = %d, want 1:\n%s", count, view)
	}
}

func TestViewShellRowsFitVeryNarrowTerminal(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 12, Height: 24})
	model = testModel(t, updated)
	model.Progress.Mode = stateStreaming
	model.Progress.Status = "Running a very long status message"
	model.InFlight.QueuedTurns = []string{strings.Repeat("queued ", 8)}
	model.Input.Composer.SetValue(strings.Repeat("composer ", 8))
	model.layout()

	view := ansi.Strip(model.View().Content)
	for i, line := range strings.Split(view, "\n") {
		if line == "" {
			continue
		}
		if got := ansi.StringWidth(line); got > model.shellWidth() {
			t.Fatalf(
				"line %d width = %d, want <= %d: %q\nview:\n%s",
				i,
				got,
				model.shellWidth(),
				line,
				view,
			)
		}
	}
}

func TestViewShellSeparatorsUseWideShellWidth(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 24})
	model = testModel(t, updated)
	model.Progress.Mode = stateReady

	view := ansi.Strip(model.View().Content)
	wantWidth := model.shellWidth()
	if wantWidth <= 24 {
		t.Fatalf("test setup shell width = %d, want > 24", wantWidth)
	}
	for _, line := range strings.Split(view, "\n") {
		if line == "" || strings.Trim(line, "─") != "" {
			continue
		}
		if got := ansi.StringWidth(line); got != wantWidth {
			t.Fatalf("wide separator width = %d, want %d: %q", got, wantWidth, line)
		}
	}
}

func TestWidthShrinkDoesNotCommitScrollbackRows(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 120, Height: 24})
	model = testModel(t, updated)
	updated, cmd := model.Update(tea.WindowSizeMsg{Width: 60, Height: 24})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("expected clear-screen command after width shrink")
	}
	if msg := cmd(); fmt.Sprintf("%T", msg) != "tea.sequenceMsg" {
		t.Fatalf("width shrink command = %T, want tea.sequenceMsg", msg)
	}

	updated, cmd = model.Update(tea.WindowSizeMsg{Width: 80, Height: 24})
	if cmd != nil {
		t.Fatalf("width growth returned command %T, want nil", cmd)
	}
	model = testModel(t, updated)
	view := ansi.Strip(model.View().Content)
	if strings.HasPrefix(view, "\n") {
		t.Fatalf("view = %q, want no leading clear rows", view)
	}
	if count := strings.Count(view, "• Ready"); count != 1 {
		t.Fatalf("ready row count = %d, want 1:\n%s", count, view)
	}
}

func TestPickerRowsFitShellWidthAfterResize(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 40, Height: 24})
	model = testModel(t, updated)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a command with a long title that must not wrap",
		query: "this is a long command picker search query that used to wrap",
		items: []pickerItem{{
			Label:  "/settings",
			Detail: "configure display settings with a long explanatory detail",
			Group:  "Commands",
		}},
		filtered: []pickerItem{{
			Label:  "/settings",
			Detail: "configure display settings with a long explanatory detail",
			Group:  "Commands",
		}},
		index:   0,
		purpose: pickerPurposeCommand,
	}

	out := ansi.Strip(model.renderPicker())
	for i, line := range strings.Split(out, "\n") {
		if got := ansi.StringWidth(line); got > model.shellWidth() {
			t.Fatalf(
				"picker line %d width = %d, want <= %d: %q\n%s",
				i,
				got,
				model.shellWidth(),
				line,
				out,
			)
		}
	}
}

func TestViewAddsBlankLineBetweenActiveContentAndShell(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateWorking
	model.Progress.Status = "Running bash..."
	model.InFlight.PendingTools = map[string]*session.Entry{
		"bash-1": {
			Role:  session.Tool,
			Title: "Bash(go test ./...)",
		},
	}

	view := ansi.Strip(model.View().Content)
	if !strings.Contains(view, "• Bash(go test ./...)\n\n") ||
		!strings.Contains(view, "Running bash...") {
		t.Fatalf("view = %q, want one blank row between active tool and shell progress", view)
	}
	if !lineAfterContainsOnly(view, "Running bash...", "─") {
		t.Fatalf("view = %q, want shell top separator below running progress", view)
	}
}

func TestViewKeepsShellTopSeparatorDuringActiveProgress(t *testing.T) {
	for _, tt := range []struct {
		name      string
		configure func(*Model)
		want      string
	}{
		{
			name: "ionizing",
			configure: func(m *Model) {
				m.Progress.Mode = stateIonizing
			},
			want: "Ionizing...",
		},
		{
			name: "streaming",
			configure: func(m *Model) {
				m.Progress.Mode = stateStreaming
			},
			want: "Streaming...",
		},
		{
			name: "working",
			configure: func(m *Model) {
				m.Progress.Mode = stateWorking
				m.Progress.Status = "Running bash..."
			},
			want: "Running bash...",
		},
		{
			name: "compacting",
			configure: func(m *Model) {
				m.Progress.Compacting = true
			},
			want: "Compacting context...",
		},
	} {
		t.Run(tt.name, func(t *testing.T) {
			model := readyModel(t)
			tt.configure(&model)

			view := ansi.Strip(model.View().Content)
			if !lineAfterContainsOnly(view, tt.want, "─") {
				t.Fatalf("view = %q, want shell top separator after %q", view, tt.want)
			}
		})
	}
}

func lineAfterContainsOnly(view, needle, chars string) bool {
	lines := strings.Split(view, "\n")
	for i, line := range lines {
		if !strings.Contains(line, needle) {
			continue
		}
		return i+1 < len(lines) && strings.Trim(lines[i+1], chars) == ""
	}
	return false
}

func TestViewAddsBlankLineBetweenQueuedTurnsAndProgress(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming
	model.InFlight.QueuedTurns = []string{"what happened?"}

	view := ansi.Strip(model.View().Content)
	if !strings.Contains(view, "Queued (Ctrl+G edit): what happened?\n\n") ||
		!strings.Contains(view, "Streaming...") {
		t.Fatalf("view = %q, want one blank row between queued turn and shell progress", view)
	}
}

func TestErrorProgressLineUsesCompactStateCopy(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateError
	model.Progress.LastError = "backend failed"

	if got := ansi.Strip(model.progressLine()); got != "× Error" {
		t.Fatalf("progress line = %q, want compact error state", got)
	}
}

func TestErrorProgressLineSuppressesDuplicateAfterTranscriptPrint(t *testing.T) {
	model := readyModel(t)
	model.App.PrintedTranscript = true
	model.Progress.Mode = stateError
	model.Progress.LastError = "backend failed"

	if got := ansi.Strip(model.progressLine()); got != "" {
		t.Fatalf("progress line = %q, want no duplicate error row", got)
	}
}

func TestRetryCountdownStatusUsesStatusTimestamp(t *testing.T) {
	updatedAt := time.Date(2026, 5, 17, 13, 0, 0, 0, time.UTC)
	status := "Provider error: upstream timeout. Retrying in 5s... Ctrl+C stops."

	got := retryCountdownStatus(status, updatedAt, updatedAt.Add(2100*time.Millisecond))
	if !strings.Contains(got, "Retrying in 3s") {
		t.Fatalf("status = %q, want countdown to 3s", got)
	}

	got = retryCountdownStatus(status, updatedAt, updatedAt.Add(6*time.Second))
	if !strings.Contains(got, "Retrying now") {
		t.Fatalf("status = %q, want retry-now state", got)
	}
}

func TestRunningProgressLinePutsElapsedAfterTokenCounters(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming
	model.Progress.TurnStartedAt = time.Now().Add(-2 * time.Second)
	model.Progress.CurrentTurnInput = 3000
	model.Progress.CurrentTurnOutput = 84

	line := ansi.Strip(model.progressLine())
	for _, want := range []string{
		"Streaming...",
		"↑ 3.0k",
		"↓ 84",
		"2s",
		"Esc/Ctrl+C to cancel",
	} {
		if !strings.Contains(line, want) {
			t.Fatalf("progress line = %q, missing %q", line, want)
		}
	}
	if strings.Index(line, "2s") < strings.Index(line, "↓ 84") {
		t.Fatalf("progress line = %q, want elapsed time after token counters", line)
	}
}

func TestRunningProgressLineUsesCyanSpinner(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming

	line := model.progressLine()
	want := model.st.cyan.Render(model.Input.Spinner.Spinner.Frames[0])
	if !strings.Contains(line, want) {
		t.Fatalf("progress line = %q, want cyan spinner %q", line, want)
	}
}

func TestStatusLineFitsWidthAfterResize(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 32, Height: 24})
	model = testModel(t, updated)
	model.Model.Backend = stubBackend{
		sess:         &stubSession{events: make(chan session.Event)},
		provider:     "subscription-provider-with-a-very-long-name",
		model:        "model-name-that-would-wrap-in-a-small-terminal",
		contextLimit: 128000,
	}
	model.Progress.TokensSent = 45123
	model.Progress.TokensReceived = 78210
	model.Progress.TotalCost = 0.042
	model.App.Workdir = "/Users/nick/github/nijaru/ion"
	model.App.Branch = "feature/resize-persistence"

	if got := lipgloss.Width(model.statusLine()); got > model.shellWidth() {
		t.Fatalf(
			"expected status line width <= %d, got %d: %q",
			model.shellWidth(),
			got,
			model.statusLine(),
		)
	}
}

func TestStatusLineStartsWithInsetSpace(t *testing.T) {
	model := readyModel(t)

	line := ansi.Strip(model.statusLine())
	if !strings.HasPrefix(line, " ") {
		t.Fatalf("status line = %q, want leading inset space", line)
	}
	if got := lipgloss.Width(model.statusLine()); got > model.shellWidth() {
		t.Fatalf(
			"expected status line width <= %d, got %d: %q",
			model.shellWidth(),
			got,
			model.statusLine(),
		)
	}
}

func TestStatusLinePendingActionStartsWithInsetSpace(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 32, Height: 24})
	model = testModel(t, updated)
	model.Input.Pending = pendingActionQuitCtrlC

	line := ansi.Strip(model.statusLine())
	if !strings.HasPrefix(line, " ") {
		t.Fatalf("pending status line = %q, want leading inset space", line)
	}
	if !strings.Contains(line, "Press Ctrl+C again to quit") {
		t.Fatalf("pending status line = %q, want quit hint", line)
	}
	if got := lipgloss.Width(model.statusLine()); got > model.shellWidth() {
		t.Fatalf(
			"expected pending status line width <= %d, got %d: %q",
			model.shellWidth(),
			got,
			model.statusLine(),
		)
	}
}

func TestStatusLineShowsWorkspacePathWithoutMode(t *testing.T) {
	model := readyModel(t)
	model.App.Workdir = "/tmp/sy"

	line := ansi.Strip(model.statusLine())
	if strings.Contains(line, "[EDIT]") {
		t.Fatalf("status line should hide stabilization-only mode: %q", line)
	}
	if !strings.Contains(line, "sy/") {
		t.Fatalf("status line missing workspace name: %q", line)
	}
	if strings.Contains(line, "./sy") {
		t.Fatalf("status line should not invent relative workspace path: %q", line)
	}
}

func TestStatusLineDoesNotShowBranchWithoutWorkspace(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 44, Height: 24})
	model = testModel(t, updated)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "short-model",
	}
	model.App.Workdir = "/tmp/very-long-workspace-name"
	model.App.Branch = "main"

	line := ansi.Strip(model.statusLine())
	if strings.Contains(line, "main") && !strings.Contains(line, "very-long-workspace-name/") {
		t.Fatalf("status line should not show branch without workspace path: %q", line)
	}
}

func TestStatusLineMarksFastPresetOnModelSegment(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "deepseek/deepseek-v4-flash:free",
	}

	line := ansi.Strip(model.statusLine())
	if strings.Contains(line, "fast") {
		t.Fatalf("primary status line should not show fast marker: %q", line)
	}

	model.App.ActivePreset = presetFast
	line = ansi.Strip(model.statusLine())
	if strings.Contains(line, "[FAST]") {
		t.Fatalf("status line should not render fast as a mode label: %q", line)
	}
	if !strings.Contains(line, "deepseek/deepseek-v4-flash:free (fast)") {
		t.Fatalf("status line should mark fast on model segment: %q", line)
	}
}

func TestStatusLineHidesZeroUsageBeforeFirstTurn(t *testing.T) {
	model := readyModel(t)
	model.Progress.TokensSent = 0
	model.Progress.TokensReceived = 0
	model.Progress.ContextTokens = 0
	model.Progress.TotalCost = 0
	model.Model.Backend = stubBackend{sess: &stubSession{events: make(chan session.Event)}}

	line := ansi.Strip(model.statusLine())
	if strings.Contains(line, "0 tokens") {
		t.Fatalf("status line should hide zero usage, got %q", line)
	}
	if strings.Contains(line, "k/") {
		t.Fatalf("status line should not show context usage without turns, got %q", line)
	}
}

func TestStatusLineColorsContextUsageByContextPercentage(t *testing.T) {
	tests := []struct {
		name  string
		total int
		want  string
	}{
		{name: "low", total: 49_000, want: "+green"},
		{name: "mid", total: 50_000, want: "+yellow"},
		{name: "high", total: 80_000, want: "+red"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			model := readyModel(t)
			model.Model.Backend = stubBackend{
				sess:         &stubSession{events: make(chan session.Event)},
				contextLimit: 100_000,
			}
			model.Progress.ContextTokens = tt.total

			label := fmt.Sprintf("%dk/100k (%d%%)", tt.total/1000, tt.total/1000)
			rendered := map[string]string{
				"+green":  model.st.success.Render(label),
				"+yellow": model.st.caution.Render(label),
				"+red":    model.st.warn.Render(label),
			}[tt.want]
			if !strings.Contains(model.statusLine(), rendered) {
				t.Fatalf("status line = %q, want rendered usage %q", model.statusLine(), rendered)
			}
		})
	}
}

func TestStatusLineDoesNotUseCumulativeTokensAsContextUsage(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:         &stubSession{events: make(chan session.Event)},
		contextLimit: 100_000,
	}
	model.Progress.TokensSent = 180_000
	model.Progress.TokensReceived = 20_000
	model.Progress.ContextTokens = 0

	line := ansi.Strip(model.statusLine())
	if strings.Contains(line, "200k/100k") {
		t.Fatalf("status line used cumulative tokens as context usage: %q", line)
	}
	if strings.Contains(line, "k/100k") {
		t.Fatalf("status line should hide unknown context usage: %q", line)
	}
}

func TestStatusLineShowsSmallContextUsageWithoutZeroK(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:         &stubSession{events: make(chan session.Event)},
		contextLimit: 128_000,
	}
	model.Progress.ContextTokens = 999

	line := ansi.Strip(model.statusLine())
	if !strings.Contains(line, "999/128k (0%)") {
		t.Fatalf("status line = %q, want exact small context usage", line)
	}
	if strings.Contains(line, "0k/") {
		t.Fatalf("status line rounded small context usage to zero: %q", line)
	}
}

func TestStatusLineShowsConfiguredSessionCostBudget(t *testing.T) {
	model := readyModel(t)
	model.Model.Config = &config.Config{MaxSessionCost: 0.25}
	model.Progress.TotalCost = 0.075

	line := ansi.Strip(model.statusLine())
	if !strings.Contains(line, "$0.075/$0.250") {
		t.Fatalf("status line missing cost budget: %q", line)
	}
}

func TestStatusLineIncludesThinkingLevel(t *testing.T) {
	model := readyModel(t)
	model.Progress.ReasoningEffort = "high"
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "o3-mini",
	}

	line := ansi.Strip(model.statusLine())
	if !strings.Contains(line, "high") {
		t.Fatalf("status line missing thinking level: %q", line)
	}
	if strings.Contains(line, "think=") {
		t.Fatalf("status line should not show the thinking key: %q", line)
	}
}

func TestStatusLineOmitsSandboxPosture(t *testing.T) {
	model := New(
		stubBackend{
			sess: &stubSession{events: make(chan session.Event)},
			surface: backend.ToolSurface{
				Count:   2,
				Names:   []string{"bash", "read"},
				Sandbox: "auto: seatbelt",
			},
		},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 120, Height: 30})
	model = testModel(t, updated)

	line := ansi.Strip(model.statusLine())
	if strings.Contains(line, "sandbox") {
		t.Fatalf("status line should leave sandbox posture to /status or /tools: %q", line)
	}
}
