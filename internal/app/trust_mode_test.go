package app

import (
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
)

func TestWithModeConfiguresSessionPolicy(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := New(stubBackend{sess: sess}, nil, nil, "/tmp/test", "main", "dev", nil).
		WithMode(session.ModeYolo)

	if model.Mode != session.ModeYolo {
		t.Fatalf("model mode = %v, want auto", model.Mode)
	}
	if sess.mode != session.ModeYolo {
		t.Fatalf("session mode = %v, want auto", sess.mode)
	}
	if !sess.autoApprove {
		t.Fatal("session auto approval was not enabled for auto mode")
	}

	model = model.WithMode(session.ModeRead)
	if sess.mode != session.ModeRead {
		t.Fatalf("session mode = %v, want read", sess.mode)
	}
	if sess.autoApprove {
		t.Fatal("session auto approval stayed enabled outside auto mode")
	}
}

func TestShiftTabDoesNotChangeModeDuringStabilization(t *testing.T) {
	model := readyModel(t)
	model.Mode = session.ModeYolo

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyTab, Mod: tea.ModShift})
	model = updated.(Model)
	if model.Mode != session.ModeYolo {
		t.Fatalf("mode = %v, want unchanged auto", model.Mode)
	}
	if cmd != nil {
		t.Fatal("shift+tab should be disabled while modes are hidden")
	}
}

func TestModeCommandsAreDisabledDuringStabilization(t *testing.T) {
	model := readyModel(t)
	model.Mode = session.ModeRead
	sess := model.Model.Session.(*stubSession)

	updated, cmd := model.handleCommand("/edit")
	model = updated
	if model.Mode != session.ModeYolo {
		t.Fatalf("mode after /edit = %v, want trusted auto", model.Mode)
	}
	if sess.mode != session.ModeYolo || !sess.autoApprove {
		t.Fatalf("session mode=%v auto=%v, want auto approval", sess.mode, sess.autoApprove)
	}
	if cmd == nil {
		t.Fatal("expected disabled mode notice")
	}
	if msg, ok := cmd().(localErrorMsg); ok {
		t.Fatalf("mode command should be a notice, got error: %v", msg.err)
	}
}

func TestTrustCommandIsCompatibilityNoop(t *testing.T) {
	model := readyModel(t).WithTrust(nil, true, "off").WithMode(session.ModeYolo)
	sess := model.Model.Session.(*stubSession)

	updated, cmd := model.handleCommand("/trust")
	model = updated
	if cmd == nil {
		t.Fatal("expected /trust command to return a notice")
	}
	if msg, ok := cmd().(localErrorMsg); ok {
		t.Fatalf("/trust should be a no-op notice: %v", msg.err)
	}
	if model.Mode != session.ModeYolo {
		t.Fatalf("mode after /trust = %v, want unchanged auto", model.Mode)
	}
	if sess.autoApprove != true {
		t.Fatal("/trust should not disable auto approval")
	}
}
