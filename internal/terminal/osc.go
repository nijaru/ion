package terminal

import (
	"fmt"
	"strings"
)

const (
	// OSC 133 semantic zones for shell integration
	OSC133PromptStart    = "\x1b]133;A\x07"
	OSC133CommandStart   = "\x1b]133;B\x07"
	OSC133OutputStart    = "\x1b]133;C\x07"
	OSC133CommandSuccess = "\x1b]133;D;0\x07"
	OSC133CommandFailure = "\x1b]133;D;1\x07"

	// OSC 9;4 tab progress states
	OSC9ProgressClear = "\x1b]9;4;0;0\x07"
	OSC9ProgressBusy  = "\x1b]9;4;1;0\x07"
	OSC9ProgressError = "\x1b]9;4;2;0\x07"
)

// SetWindowTitle returns an OSC 0 sequence to set the terminal title.
func SetWindowTitle(title string) string {
	title = strings.ReplaceAll(title, "\n", " ")
	title = strings.TrimSpace(title)
	return fmt.Sprintf("\x1b]0;%s\x07", title)
}

// ProgressSequence returns the OSC 9;4 sequence for the current agent state.
func ProgressSequence(busy, isError bool) string {
	if isError {
		return OSC9ProgressError
	}
	if busy {
		return OSC9ProgressBusy
	}
	return OSC9ProgressClear
}

// WrapUserPrompt wraps a user prompt with OSC 133 prompt and command markers.
func WrapUserPrompt(content string) string {
	return OSC133PromptStart + content + OSC133CommandStart
}

// WrapTurnOutput wraps assistant and tool outputs with OSC 133 output and exit markers.
func WrapTurnOutput(content string, isError bool) string {
	exitMarker := OSC133CommandSuccess
	if isError {
		exitMarker = OSC133CommandFailure
	}
	return OSC133OutputStart + content + exitMarker
}
