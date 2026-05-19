package app

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func applySubmitResult(t *testing.T, model Model, cmd tea.Cmd) (Model, tea.Cmd) {
	t.Helper()
	if cmd == nil {
		t.Fatal("expected submit command")
	}
	msg := cmd()
	result, ok := msg.(turnSubmitResultMsg)
	if !ok {
		t.Fatalf("submit command message = %T, want turnSubmitResultMsg", msg)
	}
	if result.err != nil {
		t.Fatalf("submit command error = %v", result.err)
	}
	updated, nextCmd := model.Update(result)
	return updated.(Model), nextCmd
}

type blockingSubmitSession struct {
	stubSession
	started chan struct{}
	release chan struct{}
}

func (s *blockingSubmitSession) SubmitTurn(ctx context.Context, turn string) error {
	close(s.started)
	select {
	case <-s.release:
	case <-ctx.Done():
		return ctx.Err()
	}
	s.submits = append(s.submits, turn)
	return nil
}

func TestSubmitTextReturnsBeforeBackendSubmitCompletes(t *testing.T) {
	sess := &blockingSubmitSession{
		stubSession: stubSession{events: make(chan session.Event)},
		started:     make(chan struct{}),
		release:     make(chan struct{}),
	}
	model := New(
		stubBackend{
			sess:     sess,
			provider: "openai",
			model:    "model-a",
		},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	type submitResult struct {
		model Model
		cmd   tea.Cmd
	}
	returned := make(chan submitResult, 1)
	go func() {
		updated, cmd := model.submitText("slow turn")
		returned <- submitResult{model: updated, cmd: cmd}
	}()

	var result submitResult
	select {
	case result = <-returned:
	case <-time.After(2 * time.Second):
		t.Fatal("submitText blocked on backend SubmitTurn")
	}
	if !result.model.InFlight.Thinking {
		t.Fatal("submitText did not mark turn in flight")
	}
	select {
	case <-sess.started:
		t.Fatal("backend SubmitTurn ran before Bubble Tea command execution")
	default:
	}

	submitted := make(chan tea.Msg, 1)
	go func() {
		submitted <- result.cmd()
	}()
	select {
	case <-sess.started:
	case <-time.After(2 * time.Second):
		t.Fatal("submit command did not call backend SubmitTurn")
	}
	select {
	case msg := <-submitted:
		t.Fatalf("submit command returned before backend completed: %T", msg)
	default:
	}

	close(sess.release)
	msg := <-submitted
	if submit, ok := msg.(turnSubmitResultMsg); !ok || submit.err != nil {
		t.Fatalf("submit result = %#v, want success", msg)
	}
	if len(sess.submits) != 1 || sess.submits[0] != "slow turn" {
		t.Fatalf("submits = %#v, want slow turn", sess.submits)
	}
}

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

	updated, cmd := model.submitText("route this")
	model = updated
	model, _ = applySubmitResult(t, model, cmd)

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

func TestSubmitTextDefersUserEchoWhenRoutingPersistenceFails(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	storageSess := &stubStorageSession{appendErr: errors.New("disk full")}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storageSess

	updated, cmd := model.submitText("keep going")
	model = updated

	model, cmd = applySubmitResult(t, model, cmd)
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(err.Error(), "persist routing decision") {
		t.Fatalf("error = %v, want routing persistence error", err)
	}
	if len(sess.submits) != 1 || sess.submits[0] != "keep going" {
		t.Fatalf("submitted turns = %#v, want keep going", sess.submits)
	}
	if model.App.PrintedTranscript {
		t.Fatal("submit should wait for ordered session event before printing user message")
	}
	var sawDecision bool
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.RoutingDecision); ok {
			sawDecision = true
			break
		}
	}
	if !sawDecision {
		t.Fatalf("missing routing decision append attempt: %#v", storageSess.appends)
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

func TestSlashCommandDoesNotEchoTranscriptEntry(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("/provider")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)

	if cmd != nil {
		t.Fatalf("command = %T, want nil for local picker without transcript echo", cmd)
	}
	if model.App.PrintedTranscript {
		t.Fatal("slash command printed a transcript entry")
	}
	if len(model.Input.History) != 1 || model.Input.History[0] != "/provider" {
		t.Fatalf("history = %#v, want /provider", model.Input.History)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeProvider {
		t.Fatalf("picker = %#v, want provider picker", model.Picker.Overlay)
	}
}

func TestSubmitComposerRejectsIncompleteRuntimeConfiguration(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Backend = stubBackend{
		sess:        sess,
		provider:    "openrouter",
		providerSet: true,
		model:       "",
		modelSet:    true,
	}
	model.Input.Composer.SetValue("hello")

	updated, cmd := model.submitComposer()
	model = updated

	if len(sess.submits) != 0 {
		t.Fatalf("submits = %v, want none for incomplete runtime", sess.submits)
	}
	if cmd == nil {
		t.Fatal("expected configuration error")
	}
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(err.Error(), "No model configured") {
		t.Fatalf("error = %v, want no model configured", err)
	}
	if got := model.Input.Composer.Value(); got != "hello" {
		t.Fatalf("composer = %q, want original prompt preserved", got)
	}
}

func TestSubmitComposerPreservesPasteMarkersWhenPromptIsBlocked(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Backend = stubBackend{
		sess:        sess,
		provider:    "openrouter",
		providerSet: true,
		model:       "",
		modelSet:    true,
	}
	placeholder := "[paste #1 +12 lines]"
	model.Input.Composer.SetValue(placeholder)
	model.PasteMarkers[placeholder] = pasteMarker{
		placeholder: placeholder,
		content:     "expanded paste content",
	}

	updated, cmd := model.submitComposer()
	model = updated

	if cmd == nil {
		t.Fatal("expected configuration error")
	}
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(err.Error(), "No model configured") {
		t.Fatalf("error = %v, want no model configured", err)
	}
	if got := model.Input.Composer.Value(); got != placeholder {
		t.Fatalf("composer = %q, want paste placeholder preserved", got)
	}
	if len(model.PasteMarkers) != 1 {
		t.Fatalf("paste markers = %#v, want preserved marker", model.PasteMarkers)
	}
	if got := model.expandMarkers(model.Input.Composer.Value()); got != "expanded paste content" {
		t.Fatalf("expanded composer = %q, want original paste content", got)
	}
}

func TestSubmitComposerConsumesPasteMarkersAfterAcceptedPrompt(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	placeholder := "[paste #1 +12 lines]"
	model.Input.Composer.SetValue("summarize " + placeholder)
	model.PasteMarkers[placeholder] = pasteMarker{
		placeholder: placeholder,
		content:     "expanded paste content",
	}

	updated, cmd := model.submitComposer()
	model = updated

	model, _ = applySubmitResult(t, model, cmd)
	if model.App.PrintedTranscript {
		t.Fatal("accepted prompt should wait for ordered session event before printing user message")
	}
	if len(sess.submits) != 1 || sess.submits[0] != "summarize expanded paste content" {
		t.Fatalf("submits = %#v, want expanded paste content", sess.submits)
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared after accepted prompt", got)
	}
	if len(model.PasteMarkers) != 0 {
		t.Fatalf("paste markers = %#v, want consumed after accepted prompt", model.PasteMarkers)
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

	updated, cmd := model.submitText("hello")
	model = updated
	model, _ = applySubmitResult(t, model, cmd)

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
	model.Progress.LastToolUseID = "tool-1"
	model.InFlight.Pending = &session.Entry{Role: session.Agent, Content: "partial"}
	model.InFlight.PendingTools = map[string]*session.Entry{
		"tool-1": {Role: session.Tool, Content: "partial tool"},
	}
	model.InFlight.Subagents = map[string]*SubagentProgress{
		"child-1": {ID: "child-1", Name: "child"},
	}
	model.InFlight.QueuedTurns = []string{"follow up"}
	model.InFlight.StreamBuf = "partial"
	model.InFlight.ReasonBuf = "reasoning"
	model.InFlight.AgentCommitted = true

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
	if model.InFlight.Pending != nil ||
		len(model.InFlight.PendingTools) != 0 ||
		len(model.InFlight.Subagents) != 0 ||
		len(model.InFlight.QueuedTurns) != 0 ||
		model.InFlight.StreamBuf != "" ||
		model.InFlight.ReasonBuf != "" ||
		model.InFlight.AgentCommitted ||
		model.Progress.LastToolUseID != "" {
		t.Fatalf("in-flight state not cleared after budget cancel: %#v", model.InFlight)
	}
	var decision storage.RoutingDecision
	var system storage.System
	for _, event := range storageSess.appends {
		switch e := event.(type) {
		case storage.RoutingDecision:
			decision = e
		case storage.System:
			system = e
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
	if !strings.Contains(system.Content, "Canceled: turn cost limit reached") {
		t.Fatalf("system cancellation = %q, want budget cancellation", system.Content)
	}
}

func TestTokenUsagePersistenceErrorStillCancelsOverBudgetTurn(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	storageSess := &stubStorageSession{appendErr: errors.New("disk full")}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storageSess
	model.Model.Config = &config.Config{MaxTurnCost: 0.01}
	model.InFlight.Thinking = true
	model.Progress.Mode = stateStreaming

	updated, cmd := model.handleSessionEvent(session.TokenUsage{Cost: 0.011})
	model = updated

	if sess.cancels != 1 {
		t.Fatalf("cancels = %d, want 1", sess.cancels)
	}
	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want cancelled", model.Progress.Mode)
	}
	requireSequenceCmd(t, cmd)
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

func TestQueuedTurnRearmsEventReaderWhenSubmissionBlocked(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Config = &config.Config{MaxSessionCost: 0.01}
	model.Progress.TotalCost = 0.01

	updated, cmd := model.handleQueuedTurn(queuedTurnMsg{
		text:               "follow up",
		rearmSessionEvents: true,
	})
	model = updated

	if len(sess.submits) != 0 {
		t.Fatalf("submitted turns = %#v, want none", sess.submits)
	}
	if model.InFlight.Thinking {
		t.Fatal("blocked queued turn should not mark the model in-flight")
	}
	requireSequenceCmd(t, cmd)
}

func TestQueuedTurnCanUseExistingEventReaderWhenSubmissionBlocked(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Config = &config.Config{MaxSessionCost: 0.01}
	model.Progress.TotalCost = 0.01

	updated, cmd := model.handleQueuedTurn(queuedTurnMsg{
		text:               "follow up",
		rearmSessionEvents: false,
	})
	model = updated

	if len(sess.submits) != 0 {
		t.Fatalf("submitted turns = %#v, want none", sess.submits)
	}
	if model.InFlight.Thinking {
		t.Fatal("blocked queued turn should not mark the model in-flight")
	}
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(
		err.Error(),
		"session cost limit reached",
	) {
		t.Fatalf("error = %v, want session cost limit", err)
	}
}

func TestCancelledTurnDrainsLateEventsUntilNextTurnStarts(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	model := readyModel(t)
	model.Model.Session = sess
	model.InFlight.Thinking = true
	model.InFlight.Pending = &session.Entry{Role: session.Agent, Content: "partial"}
	model.InFlight.StreamBuf = "partial"
	model.Progress.Status = "Running bash..."

	next, cmd := model.cancelRunningTurn("Canceled by user")
	model = next
	if cmd == nil {
		t.Fatal("expected cancellation print command")
	}
	if !model.InFlight.DrainUntilTurnStarted {
		t.Fatal("expected cancel to drain late turn events")
	}
	drainStartedAt := model.InFlight.DrainStartedAt
	if drainStartedAt.IsZero() {
		t.Fatal("expected cancel to record drain fence timestamp")
	}

	model.App.PrintedTranscript = false
	updated, _ := model.Update(session.UserMessage{
		Base:    session.BaseAt(drainStartedAt.Add(-time.Millisecond)),
		Message: "stale canceled prompt",
	})
	model = updated.(Model)
	if model.App.PrintedTranscript {
		t.Fatal("late canceled-turn user message printed transcript output")
	}

	updated, _ = model.Update(session.TurnStarted{
		Base: session.BaseAt(drainStartedAt.Add(-time.Millisecond)),
	})
	model = updated.(Model)
	if model.Progress.Mode != stateCancelled {
		t.Fatalf("late turn start reopened progress mode = %v", model.Progress.Mode)
	}
	if !model.InFlight.DrainUntilTurnStarted {
		t.Fatal("late turn start cleared drain fence")
	}
	for _, ev := range []session.Event{
		session.AgentDelta{Delta: "stale"},
		session.ThinkingDelta{Delta: "stale reasoning"},
		session.AgentMessage{Message: "stale final"},
		session.ToolCallStarted{ToolUseID: "tool-1", ToolName: "bash", Args: "echo stale"},
		session.ToolResult{ToolUseID: "tool-1", ToolName: "bash", Result: "stale"},
		session.StatusChanged{Status: "Ready"},
		session.TurnFinished{},
	} {
		updated, _ := model.Update(ev)
		model = updated.(Model)
	}

	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want stateCancelled", model.Progress.Mode)
	}
	if model.App.PrintedTranscript {
		t.Fatal("late cancelled-turn events printed transcript output")
	}
	if model.InFlight.Pending != nil ||
		model.InFlight.StreamBuf != "" ||
		model.InFlight.ReasonBuf != "" ||
		len(model.InFlight.PendingTools) != 0 ||
		model.Progress.Status != "" {
		t.Fatalf("late cancelled-turn events changed visible state: %#v", model.InFlight)
	}

	updated, cmd = model.Update(session.UserMessage{
		Base:    session.BaseAt(drainStartedAt.Add(time.Millisecond)),
		Message: "fresh prompt",
	})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("fresh user message after cancel did not print")
	}
	if model.InFlight.DrainUntilTurnStarted {
		t.Fatal("fresh user message did not clear drain fence")
	}

	updated, _ = model.Update(session.TurnStarted{
		Base: session.BaseAt(drainStartedAt.Add(time.Millisecond)),
	})
	model = updated.(Model)
	if model.InFlight.DrainUntilTurnStarted {
		t.Fatal("fresh turn did not clear drain fence")
	}
	if !model.InFlight.Thinking || model.Progress.Mode != stateIonizing {
		t.Fatalf(
			"fresh turn state = thinking %v mode %v",
			model.InFlight.Thinking,
			model.Progress.Mode,
		)
	}

	updated, _ = model.Update(session.AgentDelta{Delta: "fresh"})
	model = updated.(Model)
	if model.InFlight.Pending == nil || model.InFlight.Pending.Content != "fresh" {
		t.Fatalf("fresh turn delta not accepted: %#v", model.InFlight.Pending)
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

	updated, cmd := model.submitText("try again smaller")
	model = updated
	model, _ = applySubmitResult(t, model, cmd)

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
	sess := &stubSession{events: make(chan session.Event, 1)}
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
	submitMsg := nextCmd()
	submitResult, ok := submitMsg.(turnSubmitResultMsg)
	if !ok {
		t.Fatalf("queued follow-up command returned %T, want turnSubmitResultMsg", submitMsg)
	}
	if submitResult.err != nil || !submitResult.rearm {
		t.Fatalf("queued submit result = %#v, want successful rearmed submit", submitResult)
	}
	next, nextCmd = model.Update(submitResult)
	model = next.(Model)
	if nextCmd == nil {
		t.Fatal("expected queued submit result to re-arm session event wait")
	}
	eventResult := make(chan tea.Msg, 1)
	go func() {
		eventResult <- nextCmd()
	}()
	sess.events <- session.UserMessage{Message: "follow up"}
	rawEventMsg := <-eventResult
	eventMsg, ok := rawEventMsg.(sessionEventMsg)
	if !ok {
		t.Fatalf("queued follow-up command returned %T, want sessionEventMsg", rawEventMsg)
	}
	if len(sess.submits) != 1 || sess.submits[0] != "follow up" {
		t.Fatalf("submits = %#v, want queued follow up", sess.submits)
	}
	if _, ok := eventMsg.event.(session.UserMessage); !ok {
		t.Fatalf("queued follow-up event = %T, want UserMessage", eventMsg.event)
	}
	next, nextCmd = model.Update(eventMsg)
	model = next.(Model)
	if nextCmd == nil {
		t.Fatal("expected committed user message to print and re-arm session event wait")
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

func TestRetiredModeSlashCommandErrorsDuringTurn(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("/mode read")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none for host command", model.InFlight.QueuedTurns)
	}
	if cmd == nil {
		t.Fatal("expected retired mode command error")
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
	if cmd != nil {
		t.Fatalf("command = %T, want nil for local picker without transcript echo", cmd)
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

	if model.Progress.Mode != stateIonizing {
		t.Fatalf("progress mode before submit result = %v, want ionizing", model.Progress.Mode)
	}
	if cmd == nil {
		t.Fatal("expected submit command")
	}
	msg := cmd()
	result, ok := msg.(turnSubmitResultMsg)
	if !ok {
		t.Fatalf("submit command message = %T, want turnSubmitResultMsg", msg)
	}
	if result.err == nil || result.err.Error() != "backend unavailable" {
		t.Fatalf("submit result error = %v, want backend unavailable", result.err)
	}
	updated, cmd = model.Update(result)
	model = updated.(Model)
	if model.Progress.Mode != stateReady {
		t.Fatalf("progress mode = %v, want ready after immediate rejection", model.Progress.Mode)
	}
	if model.Progress.LastError != "" {
		t.Fatalf("last error = %q, want none for local submit rejection", model.Progress.LastError)
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
	if err := localErrorFromMsg(t, cmd()); err == nil || err.Error() != "backend unavailable" {
		t.Fatalf("local error = %v, want backend unavailable", err)
	}
	if got := model.Input.Composer.Value(); got != "hello" {
		t.Fatalf("composer = %q, want preserved draft after submit rejection", got)
	}
	if model.App.PrintedTranscript {
		t.Fatal("rejected prompt should not print a user transcript entry")
	}
	if model.InFlight.DrainUntilTurnStarted {
		t.Fatal("immediate submit rejection should not arm the session-event drain")
	}
	if len(model.Input.History) != 0 {
		t.Fatalf("history = %v, want no entry for rejected prompt", model.Input.History)
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

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated.(Model)
	model, _ = applySubmitResult(t, model, cmd)

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
