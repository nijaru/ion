package app

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestSubmitTextPersistsRoutingDecision(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	storageSess := &stubStorageSession{}
	model := New(
		stubBackend{
			sess:     sess,
			provider: "openrouter",
			model:    "anthropic/claude-sonnet-4.5",
		},
		storageSess,
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)
	model.Model.Config = &config.Config{
		MaxSessionCost: 0.25,
		MaxTurnCost:    0.05,
	}
	model.Progress.TotalCost = 0.012
	model.Progress.ReasoningEffort = "medium"

	updated, _ := model.submitText("route this")
	model = updated

	if len(sess.submits) != 1 {
		t.Fatalf("submits = %v, want one turn", sess.submits)
	}
	var decision storage.RoutingDecision
	for _, event := range storageSess.appends {
		if e, ok := event.(storage.RoutingDecision); ok {
			decision = e
			break
		}
	}
	if decision.Type == "" {
		t.Fatalf("missing routing decision in appends: %#v", storageSess.appends)
	}
	if decision.Decision != "use_model" || decision.Reason != "active_preset" {
		t.Fatalf(
			"decision = %q/%q, want use_model/active_preset",
			decision.Decision,
			decision.Reason,
		)
	}
	if decision.ModelSlot != "primary" {
		t.Fatalf("model slot = %q, want primary", decision.ModelSlot)
	}
	if decision.Provider != "openrouter" {
		t.Fatalf("provider = %q, want openrouter", decision.Provider)
	}
	if decision.Model != "anthropic/claude-sonnet-4.5" {
		t.Fatalf("model = %q, want anthropic/claude-sonnet-4.5", decision.Model)
	}
	if decision.MaxSessionCost != 0.25 || decision.MaxTurnCost != 0.05 {
		t.Fatalf(
			"budget limits = %f/%f, want 0.25/0.05",
			decision.MaxSessionCost,
			decision.MaxTurnCost,
		)
	}
	if decision.SessionCost != 0.012 {
		t.Fatalf("session cost = %f, want 0.012", decision.SessionCost)
	}
}

func TestSubmitTextDoesNotPersistSlashCommand(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		storageSess,
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	updated, _ := model.submitText("/help")
	model = updated

	if len(storageSess.appends) != 0 {
		t.Fatalf("slash command appended %d entries, want 0", len(storageSess.appends))
	}

	updated, cmd := model.handleCommand("/nope")
	model = updated
	if cmd == nil {
		t.Fatal("expected unknown slash command error")
	}
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(err.Error(), "unknown command") {
		t.Fatalf("unknown slash error = %v", err)
	}
	if len(storageSess.appends) != 0 {
		t.Fatalf("slash command error appended %d entries, want 0", len(storageSess.appends))
	}
}

func TestSlashCommandBeforeTurnDoesNotMaterializeLazySession(t *testing.T) {
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	lazy := storage.NewLazySession(store, "/tmp/test", "openai/model-a", "main")
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		lazy,
		store,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	updated, cmd := model.submitText("/help")
	model = updated
	if cmd == nil {
		t.Fatal("expected /help command")
	}
	if storage.IsMaterialized(lazy) {
		t.Fatal("slash command materialized lazy session")
	}
	recent, err := store.GetRecentSession(context.Background(), "/tmp/test")
	if err != nil {
		t.Fatalf("recent session: %v", err)
	}
	if recent != nil {
		t.Fatalf("recent session after slash command = %#v, want nil", recent)
	}
}

func TestDisplayOnlyEventBeforeTurnDoesNotMaterializeLazySession(t *testing.T) {
	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	lazy := storage.NewLazySession(store, "/tmp/test", "openai/model-a", "main")
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		lazy,
		store,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	updated, _ := model.handleSessionEvent(session.StatusChanged{Status: "Thinking..."})
	model = updated

	if storage.IsMaterialized(lazy) {
		t.Fatal("display-only event materialized lazy session")
	}
	recent, err := store.GetRecentSession(context.Background(), "/tmp/test")
	if err != nil {
		t.Fatalf("recent session: %v", err)
	}
	if recent != nil {
		t.Fatalf("recent session after display-only event = %#v, want nil", recent)
	}
}

func TestSubmitTextDoesNotPersistModelVisibleTranscript(t *testing.T) {
	storageSess := &stubStorageSession{}
	sess := &stubSession{events: make(chan session.Event)}
	model := New(
		stubBackend{sess: sess},
		storageSess,
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	updated, _ := model.submitText("hello")
	model = updated

	if len(sess.submits) != 1 || sess.submits[0] != "hello" {
		t.Fatalf("submitted turns = %#v, want hello", sess.submits)
	}
	for _, event := range storageSess.appends {
		switch event.(type) {
		case storage.User, storage.Agent, storage.ToolUse, storage.ToolResult:
			t.Fatalf("model-visible event should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestTokenUsageCancelsTurnWhenCostBudgetExceeded(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storageSess
	model.Model.Config = &config.Config{MaxTurnCost: 0.01}
	model.InFlight.Thinking = true
	model.Progress.Mode = stateStreaming

	updated, _ := model.handleSessionEvent(session.TokenUsage{
		Input:  1000,
		Output: 100,
		Cost:   0.011,
	})
	model = updated

	if sess.cancels != 1 {
		t.Fatalf("cancels = %d, want 1", sess.cancels)
	}
	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want stateCancelled", model.Progress.Mode)
	}
	if !strings.Contains(model.Progress.BudgetStopReason, "turn cost limit reached") {
		t.Fatalf("budget stop reason = %q", model.Progress.BudgetStopReason)
	}
	var decision storage.RoutingDecision
	for _, event := range storageSess.appends {
		if e, ok := event.(storage.RoutingDecision); ok {
			decision = e
			break
		}
	}
	if decision.Decision != "stop" {
		t.Fatalf("routing stop = %#v, want stop decision", decision)
	}
	if decision.StopReason != model.Progress.BudgetStopReason {
		t.Fatalf("stop reason = %q, want %q", decision.StopReason, model.Progress.BudgetStopReason)
	}
	if decision.TurnCost != 0.011 {
		t.Fatalf("turn cost = %f, want 0.011", decision.TurnCost)
	}
}

func TestTurnFinishedPreservesBudgetCancellation(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateCancelled
	model.Progress.BudgetStopReason = "turn cost limit reached ($0.011000 / $0.010000)"
	model.InFlight.QueuedTurns = []string{"next turn"}

	updated, _ := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want stateCancelled", model.Progress.Mode)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none", model.InFlight.QueuedTurns)
	}
}

func TestTurnFinishedPreservesUserCancellation(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateCancelled
	model.InFlight.QueuedTurns = []string{"next turn"}

	updated, _ := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want stateCancelled", model.Progress.Mode)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none", model.InFlight.QueuedTurns)
	}
}

func TestTurnFinishedPreservesSessionError(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateError
	model.Progress.LastError = "prompt failed"
	model.InFlight.QueuedTurns = []string{"next turn"}

	updated, _ := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want stateError", model.Progress.Mode)
	}
	if model.Progress.LastError != "prompt failed" {
		t.Fatalf("last error = %q, want prompt failed", model.Progress.LastError)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none", model.InFlight.QueuedTurns)
	}
}

func TestSubmitTextDoesNotBlockOnPriorTurnBudget(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Config = &config.Config{MaxTurnCost: 0.01}
	model.Progress.CurrentTurnCost = 0.011

	updated, _ := model.submitText("try again smaller")
	model = updated

	if len(sess.submits) != 1 {
		t.Fatalf("submitted turns = %v, want one", sess.submits)
	}
	if sess.submits[0] != "try again smaller" {
		t.Fatalf("submitted turn = %q, want retry text", sess.submits[0])
	}
}

func TestSubmitTextBlocksWhenSessionBudgetAlreadyExceeded(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Config = &config.Config{MaxSessionCost: 0.05}
	model.Progress.TotalCost = 0.05

	updated, cmd := model.submitText("do work")
	model = updated
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "session cost limit reached") {
		t.Fatalf("error = %v", err)
	}
	if len(sess.submits) != 0 {
		t.Fatalf("submitted turns = %v, want none", sess.submits)
	}
	if len(model.Input.History) != 0 {
		t.Fatalf("history = %v, want empty", model.Input.History)
	}
}

func TestCostCommandReportsMissingCost(t *testing.T) {
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		&stubStorageSession{},
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	_, cmd := model.handleCommand("/cost")
	msg := cmd()
	costMsg, ok := msg.(sessionCostMsg)
	if !ok {
		t.Fatalf("expected sessionCostMsg, got %T", msg)
	}
	if costMsg.notice != "No API cost tracked for this session" {
		t.Fatalf("cost notice = %q", costMsg.notice)
	}
}

func TestQueuedFollowUpSubmitsAfterTurnFinished(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Input.Composer.SetValue("follow up")
	model.InFlight.Thinking = true

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)
	if len(model.InFlight.QueuedTurns) != 1 || model.InFlight.QueuedTurns[0] != "follow up" {
		t.Fatalf("queuedTurns = %v, want [follow up]", model.InFlight.QueuedTurns)
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared after queueing", got)
	}
	if cmd == nil {
		t.Fatal("expected queue notice cmd")
	}

	model.InFlight.AgentCommitted = true
	updated, cmd = model.Update(session.TurnFinished{})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("expected queued turn command after finish")
	}
	msg := cmd()
	next, nextCmd := model.Update(msg)
	model = next.(Model)
	if nextCmd == nil {
		t.Fatal("expected queued turn submission command")
	}
	if len(sess.submits) != 1 || sess.submits[0] != "follow up" {
		t.Fatalf("submits = %#v, want queued follow up", sess.submits)
	}
	if got := fmt.Sprintf("%T", nextCmd()); !strings.Contains(got, "sequenceMsg") {
		t.Fatalf(
			"queued follow-up command = %s, want sequence that re-arms session event wait",
			got,
		)
	}
}

func TestBusyInputSteersDuringActiveToolWhenEnabled(t *testing.T) {
	sess := &steeringStubSession{
		stubSession: stubSession{events: make(chan session.Event)},
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Config = &config.Config{BusyInput: "steer"}
	model.Input.Composer.SetValue("use the smaller test")
	model.InFlight.Thinking = true
	model.InFlight.PendingTools = map[string]*session.Entry{
		"call-1": {Role: session.Tool, Title: "bash"},
	}

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)

	if len(sess.steers) != 1 || sess.steers[0] != "use the smaller test" {
		t.Fatalf("steers = %#v, want submitted steering", sess.steers)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none after steering", model.InFlight.QueuedTurns)
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared after steering", got)
	}
	if cmd == nil {
		t.Fatal("expected steering notice command")
	}
}

func TestBusyInputQueuesWhenSteeringHasNoToolBoundary(t *testing.T) {
	sess := &steeringStubSession{
		stubSession: stubSession{events: make(chan session.Event)},
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Config = &config.Config{BusyInput: "steer"}
	model.Input.Composer.SetValue("after this")
	model.InFlight.Thinking = true

	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)

	if len(sess.steers) != 0 {
		t.Fatalf("steers = %#v, want no steering without active tools", sess.steers)
	}
	if len(model.InFlight.QueuedTurns) != 1 || model.InFlight.QueuedTurns[0] != "after this" {
		t.Fatalf("queued turns = %#v, want fallback queue", model.InFlight.QueuedTurns)
	}
}

func TestQueuedFollowUpRendersAboveComposer(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedTurns = []string{
		"what happened?\nplease explain",
		"second queued turn",
	}

	view := ansi.Strip(model.View().Content)
	for _, want := range []string{
		"Queued (Ctrl+G edit): what happened? please explain",
		"+1 more",
		"2 queued",
	} {
		if !strings.Contains(view, want) {
			t.Fatalf("view missing %q:\n%s", want, view)
		}
	}
}

func TestCtrlGRecallsQueuedTurnsIntoComposer(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedTurns = []string{"queued one", "queued two"}
	model.Input.Composer.SetValue("draft")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'g', Mod: tea.ModCtrl})
	model = updated.(Model)

	if cmd != nil {
		t.Fatal("recall queued turns should be local")
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none", model.InFlight.QueuedTurns)
	}
	if got := model.Input.Composer.Value(); got != "draft\nqueued one\nqueued two" {
		t.Fatalf("composer = %q", got)
	}
}

func TestModeSlashCommandRunsDuringTurn(t *testing.T) {
	model := readyModel(t).WithTrust(nil, true, "prompt")
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("/mode read")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)
	if model.Mode != session.ModeRead {
		t.Fatalf("mode = %v, want read", model.Mode)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none for host command", model.InFlight.QueuedTurns)
	}
	if cmd == nil {
		t.Fatal("expected mode command notice")
	}
}

func TestSlashCommandOpensProviderPickerDuringTurn(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("/provider")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)

	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none for slash command", model.InFlight.QueuedTurns)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeProvider {
		t.Fatalf("picker = %#v, want provider picker", model.Picker.Overlay)
	}
	if cmd == nil {
		t.Fatal("expected slash command transcript print")
	}
}

func TestUnknownSlashCommandDuringTurnStaysLocal(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("/definitely-not-a-command")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)

	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none for slash command", model.InFlight.QueuedTurns)
	}
	if len(sess.submits) != 0 {
		t.Fatalf("submits = %v, want no model submit for slash command", sess.submits)
	}
	if cmd == nil {
		t.Fatal("expected local slash error command")
	}
}

func TestEscapeCancelClearsQueuedFollowUps(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	stored := &stubStorageSession{}
	model := New(stubBackend{sess: sess}, stored, nil, "/tmp/test", "main", "dev", nil)
	model.InFlight.Thinking = true
	model.InFlight.QueuedTurns = []string{"queued"}

	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyEscape})
	model = updated.(Model)

	if sess.cancels != 1 {
		t.Fatalf("cancels = %d, want 1", sess.cancels)
	}
	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want stateCancelled", model.Progress.Mode)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none", model.InFlight.QueuedTurns)
	}
}

func TestSubmitTextPropagatesImmediateSubmitErrorWithoutPersistence(t *testing.T) {
	sess := &stubSession{
		events:    make(chan session.Event),
		submitErr: errors.New("backend unavailable"),
	}
	storeSess := &stubStorageSession{id: "stub-session"}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storeSess
	model.Input.Composer.SetValue("hello")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "backend unavailable" {
		t.Fatalf("last error = %q, want backend unavailable", model.Progress.LastError)
	}
	if !model.Progress.TurnStartedAt.IsZero() {
		t.Fatalf(
			"turn started at = %v, want zero after immediate submit failure",
			model.Progress.TurnStartedAt,
		)
	}
	if len(sess.submits) != 0 {
		t.Fatalf("submit count = %d, want 0 after immediate failure", len(sess.submits))
	}
	if cmd == nil {
		t.Fatal("expected follow-up command to render transcript entries")
	}
	for _, event := range storeSess.appends {
		if _, ok := event.(storage.RoutingDecision); ok {
			t.Fatalf(
				"immediate submit error persisted routing decision %#v; failed submissions should not materialize session state",
				event,
			)
		}
		if sys, ok := event.(storage.System); ok {
			t.Fatalf(
				"immediate submit error persisted system entry %#v; local errors should not materialize transcript state",
				sys,
			)
		}
	}
}

func TestSubmitTextClearsStaleErrorImmediately(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Progress.Mode = stateError
	model.Progress.LastError = "old provider error"
	model.Progress.Status = "Running bash..."
	model.Input.Composer.SetValue("try again")

	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)

	if model.Progress.Mode != stateIonizing {
		t.Fatalf("progress mode = %v, want ionizing", model.Progress.Mode)
	}
	if model.Progress.LastError != "" {
		t.Fatalf("last error = %q, want cleared", model.Progress.LastError)
	}
	if model.Progress.Status != "" {
		t.Fatalf("status = %q, want cleared", model.Progress.Status)
	}
	if len(sess.submits) != 1 || sess.submits[0] != "try again" {
		t.Fatalf("submits = %v, want try again", sess.submits)
	}
}
