package app

import (
	"context"
	"errors"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

type thinkingTestStream struct {
	chunks []*llm.Chunk
	index  int
}

func (s *thinkingTestStream) Next() (*llm.Chunk, bool) {
	if s.index >= len(s.chunks) {
		return nil, false
	}
	chunk := s.chunks[s.index]
	s.index++
	return chunk, true
}

func (s *thinkingTestStream) Err() error   { return nil }
func (s *thinkingTestStream) Close() error { return nil }

func TestThinkingCommandUpdatesLiveRunnerAndPersistsProviderState(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	store, err := session.NewSQLiteStore(":memory:", "thinking-command")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 64)
	defer store.Close()
	var observedEffort string
	runner := agent.NewController(agent.ControllerConfig{
		Session:  sess,
		Store:    store,
		Model:    llm.Model{ID: "test"},
		Thinking: session.ThinkingLow,
		StreamFn: func(_ context.Context, req *llm.Request) (llm.Stream, error) {
			observedEffort = req.ReasoningEffort
			return &thinkingTestStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer runner.Close()

	model := New(
		stubBackend{sess: sess, provider: "openai", model: "test"},
		sess,
		store,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)
	model.Model.Runner = runner
	model.Model.Config = &config.Config{
		Provider:        "openai",
		Model:           "test",
		ReasoningEffort: "low",
	}
	model.Progress.ReasoningEffort = "low"

	updated, cmd := model.handleThinkingCommand([]string{"/thinking", "high"})
	if cmd == nil {
		t.Fatal("thinking command returned no persistence command")
	}
	msg := cmd()
	committed, ok := msg.(TransitionCommittedMsg)
	if !ok {
		t.Fatalf("thinking command message = %T, want TransitionCommittedMsg", msg)
	}
	if committed.err != nil {
		t.Fatalf("thinking persistence failed: %v", committed.err)
	}
	next, _ := updated.Update(committed)
	model = testModel(t, next)
	if model.Model.LeafID != sess.GetLeafID() {
		t.Fatalf("thinking change leaf = %q, want durable leaf %q", model.Model.LeafID, sess.GetLeafID())
	}

	if got := model.Progress.ReasoningEffort; got != "high" {
		t.Fatalf("visible reasoning effort = %q, want high", got)
	}
	state, err := config.LoadState()
	if err != nil {
		t.Fatal(err)
	}
	if state.ReasoningEffort == nil || *state.ReasoningEffort != "high" {
		t.Fatalf("saved reasoning effort = %#v, want high", state.ReasoningEffort)
	}

	if _, err := runner.Prompt(context.Background(), "verify thinking"); err != nil {
		t.Fatal(err)
	}
	if observedEffort != string(session.ThinkingHigh) {
		t.Fatalf("provider reasoning effort = %q, want high", observedEffort)
	}
	entries, err := sess.Entries(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var latest *session.ThinkingChangeEntry
	for _, entry := range entries {
		if change, ok := entry.(*session.ThinkingChangeEntry); ok {
			latest = change
		}
	}
	if latest == nil {
		t.Fatal("session did not persist a ThinkingChangeEntry")
	}
	if latest.Level != session.ThinkingHigh {
		t.Fatalf("latest session thinking level = %q, want high", latest.Level)
	}
}

func TestThinkingPersistenceFailureLeavesLiveRuntimeUnchanged(t *testing.T) {
	model := readyModel(t)
	runner := &stubRunner{}
	model.Model.Runner = runner
	model.Model.Config = &config.Config{
		Provider:        "openai",
		Model:           "test",
		ReasoningEffort: "low",
	}
	model.Progress.ReasoningEffort = "low"

	previousSave := saveRuntimeState
	saveRuntimeState = func(config.RuntimeStateUpdate) error {
		return errors.New("state unavailable")
	}
	defer func() { saveRuntimeState = previousSave }()

	updated, cmd := model.handleThinkingCommand([]string{"/thinking", "high"})
	if cmd == nil {
		t.Fatal("thinking command returned no persistence command")
	}
	msg := cmd()
	committed, ok := msg.(TransitionCommittedMsg)
	if !ok {
		t.Fatalf("thinking command message = %T, want TransitionCommittedMsg", msg)
	}
	if committed.err == nil {
		t.Fatal("thinking persistence unexpectedly succeeded")
	}
	next, _ := updated.Update(committed)
	model = testModel(t, next)

	if len(runner.thinking) != 0 {
		t.Fatalf("live thinking updates = %#v, want none", runner.thinking)
	}
	if got := model.Progress.ReasoningEffort; got != "low" {
		t.Fatalf("visible reasoning effort = %q, want low", got)
	}
	if got := model.Model.Config.ReasoningEffort; got != "low" {
		t.Fatalf("runtime reasoning effort = %q, want low", got)
	}
}

func TestFastThinkingCommandPersistsFastPresetState(t *testing.T) {
	model := readyModel(t)
	model.App.ActivePreset = PresetFast
	runner := &stubRunner{}
	model.Model.Runner = runner
	model.Model.Config = &config.Config{
		Provider:            "openai",
		FastModel:           "test",
		FastReasoningEffort: "low",
	}
	model.Progress.ReasoningEffort = "low"

	updated, cmd := model.handleThinkingCommand([]string{"/thinking", "high"})
	if cmd == nil {
		t.Fatal("thinking command returned no persistence command")
	}
	msg := cmd()
	committed, ok := msg.(TransitionCommittedMsg)
	if !ok {
		t.Fatalf("thinking command message = %T, want TransitionCommittedMsg", msg)
	}
	if committed.err != nil {
		t.Fatalf("thinking persistence failed: %v", committed.err)
	}
	next, _ := updated.Update(committed)
	model = testModel(t, next)

	state, err := config.LoadState()
	if err != nil {
		t.Fatal(err)
	}
	if state.FastReasoningEffort == nil || *state.FastReasoningEffort != "high" {
		t.Fatalf("saved fast reasoning effort = %#v, want high", state.FastReasoningEffort)
	}
	if state.ReasoningEffort != nil {
		t.Fatalf("saved primary reasoning effort = %q, want unset", *state.ReasoningEffort)
	}

	updated, cmd = model.handleThinkingCommand([]string{"/thinking", "auto"})
	if cmd == nil {
		t.Fatal("auto thinking command returned no persistence command")
	}
	msg = cmd()
	committed, ok = msg.(TransitionCommittedMsg)
	if !ok {
		t.Fatalf("auto thinking command message = %T, want TransitionCommittedMsg", msg)
	}
	if committed.err != nil {
		t.Fatalf("auto thinking persistence failed: %v", committed.err)
	}
	next, _ = updated.Update(committed)
	model = testModel(t, next)
	state, err = config.LoadState()
	if err != nil {
		t.Fatal(err)
	}
	if state.FastReasoningEffort == nil || *state.FastReasoningEffort != config.DefaultReasoningEffort {
		t.Fatalf("saved fast reasoning effort = %#v, want auto", state.FastReasoningEffort)
	}
	if len(runner.thinking) != 2 || runner.thinking[1] != session.ThinkingAuto {
		t.Fatalf("live thinking updates = %#v, want high then auto", runner.thinking)
	}
}

func TestAutoThinkingCommandPersistsProviderDefaultAndUsesItNextTurn(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	store, err := session.NewSQLiteStore(":memory:", "thinking-auto")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 64)
	defer store.Close()
	var observedEffort string
	runner := agent.NewController(agent.ControllerConfig{
		Session:  sess,
		Store:    store,
		Model:    llm.Model{ID: "test"},
		Thinking: session.ThinkingHigh,
		StreamFn: func(_ context.Context, req *llm.Request) (llm.Stream, error) {
			observedEffort = req.ReasoningEffort
			return &thinkingTestStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer runner.Close()

	model := New(
		stubBackend{sess: sess, provider: "openai", model: "test"},
		sess,
		store,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)
	model.Model.Runner = runner
	model.Model.Config = &config.Config{
		Provider:        "openai",
		Model:           "test",
		ReasoningEffort: "high",
	}
	model.Progress.ReasoningEffort = "high"

	updated, cmd := model.handleThinkingCommand([]string{"/thinking", "auto"})
	if cmd == nil {
		t.Fatal("thinking command returned no persistence command")
	}
	msg := cmd()
	committed, ok := msg.(TransitionCommittedMsg)
	if !ok {
		t.Fatalf("thinking command message = %T, want TransitionCommittedMsg", msg)
	}
	if committed.err != nil {
		t.Fatalf("thinking persistence failed: %v", committed.err)
	}
	next, _ := updated.Update(committed)
	model = testModel(t, next)

	state, err := config.LoadState()
	if err != nil {
		t.Fatal(err)
	}
	if state.ReasoningEffort == nil || *state.ReasoningEffort != config.DefaultReasoningEffort {
		t.Fatalf("saved reasoning effort = %#v, want auto", state.ReasoningEffort)
	}
	if _, err := runner.Prompt(context.Background(), "verify provider default"); err != nil {
		t.Fatal(err)
	}
	if observedEffort != "" {
		t.Fatalf("provider reasoning effort = %q, want empty", observedEffort)
	}
	entries, err := sess.Entries(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var latest *session.ThinkingChangeEntry
	for _, entry := range entries {
		if change, ok := entry.(*session.ThinkingChangeEntry); ok {
			latest = change
		}
	}
	if latest == nil || latest.Level != session.ThinkingAuto {
		t.Fatalf("latest session thinking entry = %#v, want auto", latest)
	}
}

func TestThinkingSessionPersistenceFailureRollsBackRuntimeState(t *testing.T) {
	model := readyModel(t)
	if err := config.SaveReasoningState("primary", "low"); err != nil {
		t.Fatal(err)
	}
	runner := &stubRunner{thinkingErr: errors.New("session unavailable")}
	model.Model.Runner = runner
	model.Model.Config = &config.Config{
		Provider:        "openai",
		Model:           "test",
		ReasoningEffort: "low",
	}
	model.Progress.ReasoningEffort = "low"

	updated, cmd := model.handleThinkingCommand([]string{"/thinking", "high"})
	if cmd == nil {
		t.Fatal("thinking command returned no persistence command")
	}
	msg := cmd()
	committed, ok := msg.(TransitionCommittedMsg)
	if !ok {
		t.Fatalf("thinking command message = %T, want TransitionCommittedMsg", msg)
	}
	if committed.err != nil {
		t.Fatalf("runtime-state persistence failed: %v", committed.err)
	}
	next, _ := updated.Update(committed)
	model = testModel(t, next)

	if len(runner.thinking) != 0 {
		t.Fatalf("live thinking updates = %#v, want none", runner.thinking)
	}
	if got := model.Progress.ReasoningEffort; got != "low" {
		t.Fatalf("visible reasoning effort = %q, want low", got)
	}
	state, err := config.LoadState()
	if err != nil {
		t.Fatal(err)
	}
	if state.ReasoningEffort == nil || *state.ReasoningEffort != "low" {
		t.Fatalf("rolled-back reasoning effort = %#v, want low", state.ReasoningEffort)
	}
}

func TestShiftTabThinkingPickerCommitsSelectedLevel(t *testing.T) {
	model := readyModel(t)
	runner := &stubRunner{}
	model.Model.Runner = runner
	model.Model.Config = &config.Config{
		Provider:        "openai",
		Model:           "test",
		ReasoningEffort: "low",
	}
	model.Progress.ReasoningEffort = "low"

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyTab, Mod: tea.ModShift})
	if cmd != nil {
		t.Fatal("shift+tab unexpectedly returned a command while opening picker")
	}
	model = testModel(t, updated)
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeThinking {
		t.Fatalf("shift+tab overlay = %#v, want thinking picker", model.Picker.Overlay)
	}
	model.Picker.Overlay.index = 5 // High.
	model, cmd = model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	if cmd == nil {
		t.Fatal("thinking picker returned no persistence command")
	}
	msg := cmd()
	committed, ok := msg.(TransitionCommittedMsg)
	if !ok {
		t.Fatalf("thinking picker message = %T, want TransitionCommittedMsg", msg)
	}
	if committed.err != nil {
		t.Fatalf("thinking picker persistence failed: %v", committed.err)
	}
	next, _ := model.Update(committed)
	model = testModel(t, next)
	if len(runner.thinking) != 1 || runner.thinking[0] != session.ThinkingHigh {
		t.Fatalf("live thinking updates = %#v, want high", runner.thinking)
	}
	if got := model.Progress.ReasoningEffort; got != "high" {
		t.Fatalf("visible reasoning effort = %q, want high", got)
	}
}
