package app

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func TestShiftTabDoesNothingDuringCoreStabilization(t *testing.T) {
	model := readyModel(t)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyTab, Mod: tea.ModShift})
	model = updated.(Model)
	if cmd != nil {
		t.Fatal("shift+tab should be disabled while modes are hidden")
	}
}

func TestModeCommandsAreRemovedFromTUI(t *testing.T) {
	model := readyModel(t)

	for _, command := range []string{"/mode", "/read", "/edit", "/auto", "/yolo"} {
		t.Run(command, func(t *testing.T) {
			_, cmd := model.handleCommand(command)
			if cmd == nil {
				t.Fatal("expected removed mode command to return an error")
			}
			err := localErrorFromMsg(t, cmd())
			if !strings.Contains(err.Error(), "unknown command: "+command) {
				t.Fatalf("error = %v, want unknown command", err)
			}
		})
	}
}

func TestTrustCommandIsRemovedFromTUI(t *testing.T) {
	model := readyModel(t)

	_, cmd := model.handleCommand("/trust")
	if cmd == nil {
		t.Fatal("expected removed trust command to return an error")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "unknown command: /trust") {
		t.Fatalf("error = %v, want unknown command", err)
	}
}
