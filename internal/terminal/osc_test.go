package terminal

import (
	"strings"
	"testing"
)

func TestSetWindowTitle(t *testing.T) {
	got := SetWindowTitle("ion • main")
	want := "\x1b]0;ion • main\x07"
	if got != want {
		t.Fatalf("SetWindowTitle() = %q, want %q", got, want)
	}

	gotNewline := SetWindowTitle("ion\nnew\nline")
	if strings.Contains(gotNewline, "\n") {
		t.Fatalf("SetWindowTitle() should strip newlines: %q", gotNewline)
	}
}

func TestProgressSequence(t *testing.T) {
	if got := ProgressSequence(false, false); got != OSC9ProgressClear {
		t.Errorf("ProgressSequence(false, false) = %q, want clear", got)
	}
	if got := ProgressSequence(true, false); got != OSC9ProgressBusy {
		t.Errorf("ProgressSequence(true, false) = %q, want busy", got)
	}
	if got := ProgressSequence(false, true); got != OSC9ProgressError {
		t.Errorf("ProgressSequence(false, true) = %q, want error", got)
	}
}

func TestSemanticZones(t *testing.T) {
	user := WrapUserPrompt("› prompt")
	if !strings.HasPrefix(user, OSC133PromptStart) || !strings.HasSuffix(user, OSC133CommandStart) {
		t.Fatalf("WrapUserPrompt() = %q", user)
	}

	output := WrapTurnOutput("response", false)
	if !strings.HasPrefix(output, OSC133OutputStart) || !strings.HasSuffix(output, OSC133CommandSuccess) {
		t.Fatalf("WrapTurnOutput(false) = %q", output)
	}

	outputErr := WrapTurnOutput("error", true)
	if !strings.HasSuffix(outputErr, OSC133CommandFailure) {
		t.Fatalf("WrapTurnOutput(true) = %q", outputErr)
	}
}
