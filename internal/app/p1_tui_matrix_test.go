package app

import (
	"errors"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestP1InlineScenarioMatrix(t *testing.T) {
	tests := []struct {
		name string
		run  func(*testing.T)
	}{
		{"idle launch shell frame", p1MatrixIdleLaunchShellFrame},
		{"submit stream tool and commit", p1MatrixSubmitStreamToolCommit},
		{"active progress keeps shell frame", p1MatrixActiveProgressKeepsShellFrame},
		{"queued input stays visible while active", p1MatrixQueuedInputVisible},
		{"settings command stays local while active", p1MatrixSettingsCommandLocalWhileActive},
		{"cancel during active tool settles visibly", p1MatrixCancelActiveTool},
		{"provider error settles visibly", p1MatrixProviderError},
		{"resize keeps rows wrap safe", p1MatrixResizeWrapSafe},
		{"multiline composer and paste", p1MatrixMultilineComposerAndPaste},
		{"picker overlays keep shell frame", p1MatrixPickerOverlaysKeepShellFrame},
		{"resume projection renders stored transcript", p1MatrixResumeProjection},
		{"local status keeps shell frame", p1MatrixLocalStatus},
	}
	for _, tt := range tests {
		t.Run(tt.name, tt.run)
	}
}

func p1MatrixIdleLaunchShellFrame(t *testing.T) {
	model := readyModel(t)
	view := assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "Type a message")
	assertP1ViewContains(t, view, "• Ready")
}

func p1MatrixSubmitStreamToolCommit(t *testing.T) {
	model := readyModel(t)
	model = applyP1Events(
		t, model,
		session.UserMessage{Message: "inspect workspace"},
		session.TurnStarted{},
		session.TokenUsage{Input: 12, Output: 4, Cost: 0.001},
		session.AgentDelta{Delta: "streaming answer"},
		session.ToolCallStarted{
			ToolUseID: "tool-1",
			ToolName:  "read",
			Args:      `{"file_path":"README.md"}`,
		},
		session.ToolOutputDelta{ToolUseID: "tool-1", Delta: "# ion\n"},
		session.ToolResult{
			ToolUseID: "tool-1",
			ToolName:  "read",
			Result:    "# ion\n",
		},
		session.AgentMessage{Message: "done"},
		session.TurnFinished{},
	)

	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress mode = %v, want complete", model.Progress.Mode)
	}
	if model.InFlight.Pending != nil || len(model.InFlight.PendingTools) != 0 ||
		model.InFlight.StreamBuf != "" || model.InFlight.ReasonBuf != "" {
		t.Fatalf("in-flight state not settled: %#v", model.InFlight)
	}
	view := assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "Complete")
}

func p1MatrixActiveProgressKeepsShellFrame(t *testing.T) {
	model := readyModel(t)
	model = applyP1Events(
		t, model,
		session.TurnStarted{},
		session.StatusChanged{Status: "Running tool..."},
		session.TokenUsage{Input: 12000, Output: 6000, Total: 18000, Cost: 0.002},
		session.AgentDelta{Delta: "working"},
		session.ToolCallStarted{
			ToolUseID: "tool-1",
			ToolName:  "bash",
			Args:      `{"command":"sleep 2; echo ion-tmux-smoke"}`,
		},
		session.ToolOutputDelta{ToolUseID: "tool-1", Delta: "ion-tmux-"},
	)

	view := assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "Running tool")
	assertP1ViewContains(t, view, "Bash(sleep 2; echo ion-tmux-smoke)")
	assertP1ViewContains(t, view, "Type a message")
	assertP1ViewContains(t, view, "stub-model")
	assertP1ViewContains(t, view, "18k tokens")
}

func p1MatrixQueuedInputVisible(t *testing.T) {
	model := readyModel(t)
	model = applyP1Events(t, model, session.TurnStarted{})
	model.Input.Composer.SetValue("follow up after this")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)

	if cmd == nil {
		t.Fatal("queued input should print a queued notice")
	}
	if len(model.InFlight.QueuedTurns) != 1 ||
		model.InFlight.QueuedTurns[0] != "follow up after this" {
		t.Fatalf("queued turns = %#v", model.InFlight.QueuedTurns)
	}
	view := assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "Queued (Ctrl+G edit): follow up after this")
	assertP1ViewContains(t, view, "1 queued")
}

func p1MatrixSettingsCommandLocalWhileActive(t *testing.T) {
	model := readyModel(t)
	model = applyP1Events(t, model, session.TurnStarted{})
	model.Input.Composer.SetValue("/settings")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)

	if cmd != nil {
		t.Fatalf("settings picker command = %T, want nil local picker", cmd)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf(
			"queued turns = %#v, want none for local settings command",
			model.InFlight.QueuedTurns,
		)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeSettings {
		t.Fatalf("picker overlay = %#v, want settings picker", model.Picker.Overlay)
	}
	view := assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "Settings")
	assertP1ViewContains(t, view, "Busy input")
	assertP1ViewNotContains(t, view, "Queued follow-up")
	assertP1ViewNotContains(t, view, "› /settings")
}

func p1MatrixCancelActiveTool(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model = applyP1Events(
		t, model,
		session.TurnStarted{},
		session.ToolCallStarted{
			ToolUseID: "tool-1",
			ToolName:  "bash",
			Args:      `{"command":"sleep 60"}`,
		},
	)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEscape})
	model = testModel(t, updated)

	if cmd == nil {
		t.Fatal("cancel should return a command")
	}
	if model.Progress.Mode != stateCancelled || !model.InFlight.Canceling {
		t.Fatalf("cancel state = progress %#v in-flight %#v", model.Progress, model.InFlight)
	}
	view := assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "Canceled")
}

func p1MatrixProviderError(t *testing.T) {
	model := readyModel(t)
	model = applyP1Events(
		t, model,
		session.TurnStarted{},
		session.Error{Err: errors.New("provider failed while streaming")},
	)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if !model.App.PrintedTranscript {
		t.Fatal("provider error did not commit a terminal transcript entry")
	}
	assertP1ShellFrame(t, model)
}

func p1MatrixResizeWrapSafe(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 44, Height: 18})
	model = testModel(t, updated)
	model = applyP1Events(
		t, model,
		session.TurnStarted{},
		session.StatusChanged{Status: strings.Repeat("streaming very long status ", 4)},
		session.AgentDelta{Delta: strings.Repeat("long streamed output ", 8)},
		session.ToolCallStarted{
			ToolUseID: "tool-1",
			ToolName:  "grep",
			Args:      `{"pattern":"very long search pattern that wraps"}`,
		},
	)

	assertP1ShellFrame(t, model)
}

func p1MatrixMultilineComposerAndPaste(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("first line\nsecond line")
	view := assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "› first line")
	assertP1ViewContains(t, view, "  second line")
	assertP1ViewNotContains(t, view, "› second line")

	paste := strings.Repeat("large pasted line\n", pasteMarkerMinLines)
	updated, _ := model.Update(tea.PasteMsg{Content: paste})
	model = testModel(t, updated)
	if len(model.PasteMarkers) != 1 {
		t.Fatalf("paste markers = %#v, want one collapsed marker", model.PasteMarkers)
	}
	view = assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "[paste #1 +")
}

func p1MatrixPickerOverlaysKeepShellFrame(t *testing.T) {
	model := readyModel(t)
	model = model.openCommandPicker("/")
	view := assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "Pick a command")

	item := sessionPickerItem{info: storage.SessionInfo{
		ID:          "session-1",
		CWD:         model.App.Workdir,
		Model:       "fake/model",
		Branch:      "main",
		Title:       "inspect workspace",
		LastPreview: "done",
		UpdatedAt:   time.Now(),
	}}
	model.Picker.Overlay = nil
	model.Picker.Session = &sessionPickerState{
		items:    []sessionPickerItem{item},
		filtered: []sessionPickerItem{item},
	}
	view = assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "Resume a session")
	assertP1ViewContains(t, view, "inspect workspace")
}

func p1MatrixResumeProjection(t *testing.T) {
	model := readyModel(t)
	rendered := model.RenderEntries(
		session.Entry{Role: session.User, Content: "read status"},
		session.Entry{Role: session.Tool, Title: "read ai/STATUS.md", Content: "phase\nfocus\n"},
		session.Entry{Role: session.Agent, Content: "status loaded"},
	)

	joined := ansi.Strip(strings.Join(rendered, "\n"))
	assertP1ViewContains(t, joined, "› read status")
	assertP1ViewContains(t, joined, "Read(ai/STATUS.md)")
	assertP1ViewContains(t, joined, "status loaded")
}

func p1MatrixLocalStatus(t *testing.T) {
	model := readyModel(t)
	model.progressReducer().beginLocalStatus("Loading settings...")

	view := assertP1ShellFrame(t, model)
	assertP1ViewContains(t, view, "Loading settings")
}

func applyP1Events(t *testing.T, model Model, events ...session.Event) Model {
	t.Helper()
	for _, ev := range events {
		updated, _ := model.Update(ev)
		model = testModel(t, updated)
	}
	return model
}

func assertP1ShellFrame(t *testing.T, model Model) string {
	t.Helper()
	view := ansi.Strip(model.View().Content)
	separatorCount := 0
	for i, line := range strings.Split(view, "\n") {
		if line == "" {
			continue
		}
		width := ansi.StringWidth(line)
		if width > model.shellWidth() {
			t.Fatalf(
				"line %d width = %d, want <= %d: %q\nview:\n%s",
				i,
				width,
				model.shellWidth(),
				line,
				view,
			)
		}
		if strings.Trim(line, "─") == "" {
			separatorCount++
			if width != model.shellWidth() {
				t.Fatalf("separator width = %d, want %d: %q", width, model.shellWidth(), line)
			}
		}
	}
	if separatorCount != 2 {
		t.Fatalf("separator count = %d, want 2:\n%s", separatorCount, view)
	}
	return view
}

func assertP1ViewContains(t *testing.T, view, want string) {
	t.Helper()
	if !strings.Contains(view, want) {
		t.Fatalf("view missing %q:\n%s", want, view)
	}
}

func assertP1ViewNotContains(t *testing.T, view, needle string) {
	t.Helper()
	if strings.Contains(view, needle) {
		t.Fatalf("view unexpectedly contains %q:\n%s", needle, view)
	}
}
