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
	model = updated.(Model)
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

func TestViewShellSeparatorsUseWideShellWidth(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 24})
	model = updated.(Model)
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

func TestWidthShrinkRequestsBlankScrollbackRows(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 120, Height: 24})
	model = updated.(Model)
	updated, cmd := model.Update(tea.WindowSizeMsg{Width: 60, Height: 24})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("expected blank-line command after width shrink")
	}
	if msg := cmd(); fmt.Sprintf("%T", msg) != "tea.sequenceMsg" {
		t.Fatalf("resize command = %T, want tea.sequenceMsg", msg)
	}

	updated, cmd = model.Update(tea.WindowSizeMsg{Width: 80, Height: 24})
	if cmd != nil {
		t.Fatal("expected no clear-screen command after width growth")
	}
	model = updated.(Model)
	view := ansi.Strip(model.View().Content)
	if strings.HasPrefix(view, "\n") {
		t.Fatalf("view = %q, want no leading clear rows", view)
	}
}

func TestPickerRowsFitShellWidthAfterResize(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 40, Height: 24})
	model = updated.(Model)
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

func TestRunningProgressLinePutsElapsedAfterTokenCounters(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming
	model.Progress.TurnStartedAt = time.Now().Add(-2 * time.Second)
	model.Progress.CurrentTurnInput = 3000
	model.Progress.CurrentTurnOutput = 84

	line := ansi.Strip(model.progressLine())
	for _, want := range []string{"Streaming...", "↑ 3.0k", "↓ 84", "2s", "Esc to cancel"} {
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
	model = updated.(Model)
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

func TestStatusLineHidesZeroUsageBeforeFirstTurn(t *testing.T) {
	model := readyModel(t)
	model.Progress.TokensSent = 0
	model.Progress.TokensReceived = 0
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

func TestStatusLineColorsTokenUsageByContextPercentage(t *testing.T) {
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
			model.Progress.TokensSent = tt.total

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

func TestStatusLineIncludesSandboxPosture(t *testing.T) {
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
	model = updated.(Model)

	line := ansi.Strip(model.statusLine())
	if !strings.Contains(line, "sandbox auto: seatbelt") {
		t.Fatalf("status line missing sandbox posture: %q", line)
	}
}
