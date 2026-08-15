package app

import (
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/terminal"
)

func TestOSCTitleAndProgressInView(t *testing.T) {
	model := readyModel(t)
	model.App.SessionName = "test-session"

	view := model.View().Content
	if !strings.Contains(view, "\x1b]0;ion • test-session\x07") {
		t.Fatalf("View() should contain window title OSC sequence: %q", view)
	}
	if !strings.Contains(view, terminal.OSC9ProgressClear) {
		t.Fatalf("View() should contain clear progress when idle: %q", view)
	}

	// Set in flight
	model.InFlight.ReasonBuf = "thinking..."
	viewBusy := model.View().Content
	if !strings.Contains(viewBusy, "\x1b]0;ion [busy] • test-session\x07") {
		t.Fatalf("View() should show busy window title: %q", viewBusy)
	}
	if !strings.Contains(viewBusy, terminal.OSC9ProgressBusy) {
		t.Fatalf("View() should contain busy progress when active: %q", viewBusy)
	}
}

func TestOSC133SemanticZonesInTranscript(t *testing.T) {
	model := readyModel(t)

	userEntry := testUserEntry("hello world")
	userRendered := model.renderEntry(userEntry)
	if !strings.HasPrefix(userRendered, terminal.OSC133PromptStart) || !strings.HasSuffix(userRendered, terminal.OSC133CommandStart) {
		t.Fatalf("user entry should be wrapped with OSC 133 prompt markers: %q", userRendered)
	}

	agentEntry := testAgentEntry("this is the response", "")
	agentRendered := model.renderEntry(agentEntry)
	if !strings.HasPrefix(agentRendered, terminal.OSC133OutputStart) || !strings.HasSuffix(agentRendered, terminal.OSC133CommandSuccess) {
		t.Fatalf("agent entry should be wrapped with OSC 133 output markers: %q", agentRendered)
	}
}
