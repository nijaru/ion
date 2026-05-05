package app

import (
	"path/filepath"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
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

func TestShiftTabTogglesReadAndEditOnly(t *testing.T) {
	model := readyModel(t).WithTrust(nil, true, "prompt")
	model.Mode = session.ModeRead

	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyTab, Mod: tea.ModShift})
	model = updated.(Model)
	if model.Mode != session.ModeEdit {
		t.Fatalf("mode = %v, want edit", model.Mode)
	}

	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyTab, Mod: tea.ModShift})
	model = updated.(Model)
	if model.Mode != session.ModeRead {
		t.Fatalf("mode = %v, want read", model.Mode)
	}

	model.Mode = session.ModeYolo
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyTab, Mod: tea.ModShift})
	model = updated.(Model)
	if model.Mode != session.ModeEdit {
		t.Fatalf("auto Shift+Tab mode = %v, want edit", model.Mode)
	}
}

func TestShiftTabAllowsModeChangesWhenTrustStoreUnavailable(t *testing.T) {
	model := readyModel(t).WithTrust(nil, false, "prompt")
	model.Mode = session.ModeRead
	sess := model.Model.Session.(*stubSession)

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyTab, Mod: tea.ModShift})
	model = updated.(Model)
	if model.Mode != session.ModeEdit {
		t.Fatalf("mode = %v, want edit", model.Mode)
	}
	if sess.mode != session.ModeEdit {
		t.Fatalf("session mode = %v, want edit", sess.mode)
	}
	if cmd == nil {
		t.Fatal("expected Shift+Tab edit attempt to return a mode notice")
	}
	if _, ok := cmd().(localErrorMsg); ok {
		t.Fatal("missing workspace trust store should not block mode changes")
	}
}

func TestMissingWorkspaceTrustStoreAllowsEditAndAutoModes(t *testing.T) {
	model := readyModel(t).WithTrust(nil, false, "prompt")
	model.Mode = session.ModeRead

	updated, cmd := model.handleCommand("/edit")
	model = updated
	if model.Mode != session.ModeEdit {
		t.Fatalf("mode after /edit = %v, want edit", model.Mode)
	}
	if cmd == nil {
		t.Fatal("expected /edit command to return a notice")
	}
	if _, ok := cmd().(localErrorMsg); ok {
		t.Fatal("missing workspace trust store should not block /edit")
	}

	updated, cmd = model.handleCommand("/mode auto")
	model = updated
	if model.Mode != session.ModeYolo {
		t.Fatalf("mode after /mode auto = %v, want auto", model.Mode)
	}
	if cmd == nil {
		t.Fatal("expected /mode auto command to return a notice")
	}
	if _, ok := cmd().(localErrorMsg); ok {
		t.Fatal("missing workspace trust store should not block /mode auto")
	}

	updated, cmd = model.handleCommand("/read")
	model = updated
	if model.Mode != session.ModeRead {
		t.Fatalf("mode after /read = %v, want read", model.Mode)
	}
	if cmd == nil {
		t.Fatal("expected /read command to return a notice")
	}
}

func TestWorkspaceTrustGateBlocksEditAndAutoModes(t *testing.T) {
	model := readyModel(t).WithTrust(
		ionworkspace.NewTrustStore(filepath.Join(t.TempDir(), "trusted.json")),
		false,
		"prompt",
	)
	model.Mode = session.ModeRead

	updated, cmd := model.handleCommand("/edit")
	model = updated
	if model.Mode != session.ModeRead {
		t.Fatalf("mode after blocked /edit = %v, want read", model.Mode)
	}
	if cmd == nil {
		t.Fatal("expected /edit command to return an error")
	}
	if _, ok := cmd().(localErrorMsg); !ok {
		t.Fatal("untrusted workspace should block /edit")
	}

	updated, cmd = model.handleCommand("/mode auto")
	model = updated
	if model.Mode != session.ModeRead {
		t.Fatalf("mode after blocked /mode auto = %v, want read", model.Mode)
	}
	if cmd == nil {
		t.Fatal("expected /mode auto command to return an error")
	}
	if _, ok := cmd().(localErrorMsg); !ok {
		t.Fatal("untrusted workspace should block /mode auto")
	}
}

func TestTrustCommandTrustsWorkspaceAndEnablesEditMode(t *testing.T) {
	store := ionworkspace.NewTrustStore(filepath.Join(t.TempDir(), "trusted.json"))
	model := readyModel(t).WithTrust(store, false, "prompt")
	model.Mode = session.ModeRead
	sess := model.Model.Session.(*stubSession)

	updated, cmd := model.handleCommand("/trust")
	model = updated
	if cmd == nil {
		t.Fatal("expected /trust command to return a notice")
	}
	if msg, ok := cmd().(localErrorMsg); ok {
		t.Fatalf("/trust should succeed for prompt workspace trust: %v", msg.err)
	}
	if !model.App.TrustedWorkspace {
		t.Fatal("workspace did not become trusted")
	}
	if model.Mode != session.ModeEdit {
		t.Fatalf("mode after /trust = %v, want edit", model.Mode)
	}
	if sess.mode != session.ModeEdit {
		t.Fatalf("session mode after /trust = %v, want edit", sess.mode)
	}
	trusted, err := store.IsTrusted(model.App.Workdir)
	if err != nil {
		t.Fatalf("load trust state: %v", err)
	}
	if !trusted {
		t.Fatal("trust store did not persist workspace")
	}
}
