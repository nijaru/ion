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
	"github.com/nijaru/ion/session"
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
	return testModel(t, updated), nextCmd
}

type blockingSubmitSession struct {
	stubSession
	started chan struct{}
	release chan struct{}
}

type materializingSubmitSession struct {
	stubSession
	lazy *session.LazySession
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

func (s *materializingSubmitSession) SubmitTurn(ctx context.Context, turn string) error {
	if _, err := s.lazy.Ensure(ctx); err != nil {
		return err
	}
	return s.stubSession.SubmitTurn(ctx, turn)
}

type blockingSteeringSession struct {
	steeringStubSession
	started chan struct{}
	release chan struct{}
}

func (s *blockingSteeringSession) SteerTurn(
	ctx context.Context,
	text string,
) (session.SteeringResult, error) {
	close(s.started)
	select {
	case <-s.release:
	case <-ctx.Done():
		return session.SteeringResult{}, ctx.Err()
	}
	s.steers = append(s.steers, text)
	return session.SteeringResult{Outcome: session.SteeringAccepted}, nil
}

func TestSubmitTextReturnsBeforeBackendSubmitCompletes(t *testing.T) {
	sess := &blockingSubmitSession{
		stubSession: stubSession{events: make(chan session.AgentEvent)},
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

func TestSubmitTurnCmdReportsMissingSession(t *testing.T) {
	msg := submitTurnCmd(nil, "hello", "hello draft")()
	result, ok := msg.(turnSubmitResultMsg)
	if !ok {
		t.Fatalf("submit command message = %T, want turnSubmitResultMsg", msg)
	}
	if result.err == nil || !strings.Contains(result.err.Error(), "session unavailable") {
		t.Fatalf("submit command error = %v, want missing-session error", result.err)
	}
	if result.text != "hello" || result.draft != "hello draft" {
		t.Fatalf("submit result = %#v, want original text and draft", result)
	}
}

func TestAwaitSessionEventReportsMissingSession(t *testing.T) {
	model := readyModel(t)
	model.Model.Session = nil

	msg := model.awaitSessionEvent()()
	eventMsg, ok := msg.(sessionEventMsg)
	if !ok {
		t.Fatalf("await message = %T, want sessionEventMsg", msg)
	}
	errEvent, ok := eventMsg.event.(session.ErrorEvent)
	if !ok {
		t.Fatalf("session event = %T, want session.ErrorEvent", eventMsg.event)
	}
	if errEvent.Err == nil || !strings.Contains(errEvent.Err.Error(), "session unavailable") {
		t.Fatalf("session error = %v, want missing-session error", errEvent.Err)
	}
	if !errEvent.Fatal {
		t.Fatal("missing session error should be fatal")
	}
}

func TestAwaitSessionEventReportsMissingEventStream(t *testing.T) {
	model := readyModel(t)
	model.Model.Session = &stubSession{}

	msg := model.awaitSessionEvent()()
	eventMsg, ok := msg.(sessionEventMsg)
	if !ok {
		t.Fatalf("await message = %T, want sessionEventMsg", msg)
	}
	errEvent, ok := eventMsg.event.(session.ErrorEvent)
	if !ok {
		t.Fatalf("session event = %T, want session.ErrorEvent", eventMsg.event)
	}
	if errEvent.Err == nil ||
		!strings.Contains(errEvent.Err.Error(), "session event stream unavailable") {
		t.Fatalf("session error = %v, want missing-stream error", errEvent.Err)
	}
	if !errEvent.Fatal {
		t.Fatal("missing event stream error should be fatal")
	}
}

func TestBusyInputReturnsBeforeSteeringCompletes(t *testing.T) {
	sess := &blockingSteeringSession{
		steeringStubSession: steeringStubSession{
			stubSession: stubSession{events: make(chan session.AgentEvent)},
		},
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Config = &config.Config{BusyInput: "steer"}
	model.Input.Composer.SetValue("steer slowly")
	model.InFlight.Thinking = true
	model.InFlight.PendingTools = map[string]*session.Entry{
		"call-1": {Role: session.RoleTool, Title: "bash"},
	}

	type busyResult struct {
		model Model
		cmd   tea.Cmd
	}
	returned := make(chan busyResult, 1)
	go func() {
		updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
		returned <- busyResult{model: testModel(t, updated), cmd: cmd}
	}()

	var result busyResult
	select {
	case result = <-returned:
	case <-time.After(2 * time.Second):
		t.Fatal("busy input blocked on steering")
	}
	if got := result.model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared while steering command runs", got)
	}
	select {
	case <-sess.started:
		t.Fatal("steering ran before Bubble Tea command execution")
	default:
	}

	steered := make(chan tea.Msg, 1)
	go func() {
		steered <- result.cmd()
	}()
	select {
	case <-sess.started:
	case <-time.After(2 * time.Second):
		t.Fatal("steering command did not call SteerTurn")
	}
	select {
	case msg := <-steered:
		t.Fatalf("steering command returned before SteerTurn completed: %T", msg)
	default:
	}

	close(sess.release)
	msg := <-steered
	resultMsg, ok := msg.(steeringResultMsg)
	if !ok || resultMsg.err != nil ||
		resultMsg.result.Outcome != session.SteeringAccepted {
		t.Fatalf("steering result = %#v, want accepted", msg)
	}
	if len(sess.steers) != 1 || sess.steers[0] != "steer slowly" {
		t.Fatalf("steers = %#v, want steer slowly", sess.steers)
	}
}

func TestSubmitTextPersistsRoutingDecision(t *testing.T) {
	sess := &stubSession{events: make(chan session.AgentEvent, 1)}
	sess.events <- session.UserMessageEvent{Message: "route this"}
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
	model, cmd = applySubmitResult(t, model, cmd)

	if len(sess.submits) != 1 {
		t.Fatalf("submits = %v, want one turn", sess.submits)
	}
	if len(storageSess.appends) != 0 {
		t.Fatalf("appends before command execution = %#v, want none", storageSess.appends)
	}
	messages := runCommandTree(t, cmd)
	if !containsSessionEvent[session.UserMessageEvent](messages) {
		t.Fatalf("messages = %#v, want submitted user session event", messages)
	}
	var decision session.StoreRoutingDecision
	for _, event := range storageSess.appends {
		if e, ok := event.(session.StoreRoutingDecision); ok {
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

func TestSubmitTextRefreshesRuntimeSnapshotAfterLazySessionMaterializes(t *testing.T) {
	store, err := session.NewEphemeralCantoStore()
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	t.Cleanup(func() { _ = store.Close() })

	lazy := session.NewLazySession(store, "/tmp/test", "openai/gpt-4.1", "main")
	sess := &materializingSubmitSession{
		stubSession: stubSession{events: make(chan session.AgentEvent)},
		lazy:        lazy,
	}
	model := New(
		stubBackend{
			sess:     sess,
			provider: "openai",
			model:    "gpt-4.1",
		},
		lazy,
		store,
		"/tmp/test",
		"main",
		"dev",
		nil,
	).WithConfig(&config.Config{Provider: "openai", Model: "gpt-4.1"})

	if got := model.Model.Runtime.MaterializedSessionID(); got != "" {
		t.Fatalf("snapshot session id before submit = %q, want empty", got)
	}

	updated, cmd := model.submitText("hello")
	model = updated
	model, _ = applySubmitResult(t, model, cmd)

	if !session.IsMaterialized(lazy) {
		t.Fatal("lazy session did not materialize during submit")
	}
	if got := model.Model.Runtime.MaterializedSessionID(); got != lazy.ID() {
		t.Fatalf("snapshot session id = %q, want %q", got, lazy.ID())
	}
	if got := model.ResumeSessionID(); got != lazy.ID() {
		t.Fatalf("resume session id = %q, want %q", got, lazy.ID())
	}
}

func TestSubmitErrorRefreshesRuntimeSnapshotAfterLazySessionMaterializes(t *testing.T) {
	store, err := session.NewEphemeralCantoStore()
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	t.Cleanup(func() { _ = store.Close() })

	lazy := session.NewLazySession(store, "/tmp/test", "openai/gpt-4.1", "main")
	sess := &materializingSubmitSession{
		stubSession: stubSession{
			events:    make(chan session.AgentEvent),
			submitErr: errors.New("provider rejected turn"),
		},
		lazy: lazy,
	}
	model := New(
		stubBackend{
			sess:     sess,
			provider: "openai",
			model:    "gpt-4.1",
		},
		lazy,
		store,
		"/tmp/test",
		"main",
		"dev",
		nil,
	).WithConfig(&config.Config{Provider: "openai", Model: "gpt-4.1"})

	updated, cmd := model.submitText("hello")
	model = updated
	msg := cmd()
	result, ok := msg.(turnSubmitResultMsg)
	if !ok {
		t.Fatalf("submit result = %T, want turnSubmitResultMsg", msg)
	}
	if result.err == nil {
		t.Fatal("submit result error is nil")
	}
	next, _ := model.Update(result)
	model = testModel(t, next)

	if !session.IsMaterialized(lazy) {
		t.Fatal("lazy session did not materialize during submit")
	}
	if got := model.Model.Runtime.MaterializedSessionID(); got != lazy.ID() {
		t.Fatalf("snapshot session id = %q, want %q", got, lazy.ID())
	}
}

func TestSubmitResultArmsEventReader(t *testing.T) {
	sess := &stubSession{events: make(chan session.AgentEvent, 1)}
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storageSess

	updated, submitCmd := model.submitText("hello")
	model = updated
	if !model.InFlight.Thinking {
		t.Fatal("submit should mark turn in flight before command execution")
	}

	submitMsg := submitCmd()
	result, ok := submitMsg.(turnSubmitResultMsg)
	if !ok || result.err != nil {
		t.Fatalf("submit result = %#v, want success", submitMsg)
	}
	sess.events <- session.UserMessageEvent{Message: "hello"}

	updatedModel, cmd := model.Update(result)
	model = testModel(t, updatedModel)
	messages := runCommandTree(t, cmd)
	if !containsSessionEvent[session.UserMessageEvent](messages) {
		t.Fatalf("messages = %#v, want initial submit to arm session event reader", messages)
	}
	if len(sess.submits) != 1 || sess.submits[0] != "hello" {
		t.Fatalf("submits = %#v, want hello", sess.submits)
	}
	if len(storageSess.appends) == 0 {
		t.Fatal("expected routing decision persistence command to run")
	}
}

func TestSubmitRoutingPersistenceReturnsBeforeStorageAppendCompletes(t *testing.T) {
	sess := &stubSession{events: make(chan session.AgentEvent)}
	storageSess := &blockingAppendStorage{
		entered: make(chan any, 1),
		release: make(chan struct{}),
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storageSess

	updated, submitCmd := model.submitText("route this")
	model = updated
	submitMsg := submitCmd()
	result, ok := submitMsg.(turnSubmitResultMsg)
	if !ok || result.err != nil {
		t.Fatalf("submit result = %#v, want successful turnSubmitResultMsg", submitMsg)
	}
	updatedModel, cmd := model.Update(result)
	model = testModel(t, updatedModel)

	if len(sess.submits) != 1 || sess.submits[0] != "route this" {
		t.Fatalf("submits = %#v, want route this", sess.submits)
	}
	select {
	case event := <-storageSess.entered:
		t.Fatalf("append ran during submit result Update: %#v", event)
	default:
	}
	if len(storageSess.appends) != 0 {
		t.Fatalf("appends during submit result Update = %#v, want none", storageSess.appends)
	}

	done := make(chan []tea.Msg, 1)
	go func() {
		done <- runCommandTree(t, cmd)
	}()
	select {
	case event := <-storageSess.entered:
		decision, ok := event.(session.StoreRoutingDecision)
		if !ok {
			t.Fatalf("append event = %#v, want routing decision", event)
		}
		if decision.Decision != "use_model" || decision.Reason != "active_preset" {
			t.Fatalf("routing decision = %#v, want active preset model use", decision)
		}
	case <-time.After(time.Second):
		t.Fatal("routing persistence command did not start append")
	}
	close(storageSess.release)
	sess.events <- session.UserMessageEvent{Message: "route this"}
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("routing persistence command did not finish")
	}
	if len(storageSess.appends) != 1 {
		t.Fatalf("appends after command = %#v, want one routing decision", storageSess.appends)
	}
}

func TestSubmitTextDefersUserEchoWhenRoutingPersistenceFails(t *testing.T) {
	sess := &stubSession{events: make(chan session.AgentEvent, 1)}
	sess.events <- session.UserMessageEvent{Message: "keep going"}
	storageSess := &stubStorageSession{appendErr: errors.New("disk full")}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storageSess

	updated, cmd := model.submitText("keep going")
	model = updated

	model, cmd = applySubmitResult(t, model, cmd)
	msgs := runCommandTree(t, cmd)
	var foundPersistError bool
	for _, msg := range msgs {
		if local, ok := msg.(localErrorMsg); ok &&
			strings.Contains(local.err.Error(), "persist routing decision") {
			foundPersistError = true
		}
	}
	if !foundPersistError {
		t.Fatalf("command messages = %#v, want routing persistence error", msgs)
	}
	if len(sess.submits) != 1 || sess.submits[0] != "keep going" {
		t.Fatalf("submitted turns = %#v, want keep going", sess.submits)
	}
	if model.App.PrintedTranscript {
		t.Fatal("submit should wait for ordered session event before printing user message")
	}
	var sawDecision bool
	for _, event := range storageSess.appends {
		if _, ok := event.(session.StoreRoutingDecision); ok {
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
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
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
	store, err := session.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	lazy := session.NewLazySession(store, "/tmp/test", "openai/model-a", "main")
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
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
	if session.IsMaterialized(lazy) {
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
	model = testModel(t, updated)

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
	sess := &stubSession{events: make(chan session.AgentEvent)}
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
	sess := &stubSession{events: make(chan session.AgentEvent)}
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
	sess := &stubSession{events: make(chan session.AgentEvent)}
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
		t.Fatal(
			"accepted prompt should wait for ordered session event before printing user message",
		)
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
	store, err := session.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	lazy := session.NewLazySession(store, "/tmp/test", "openai/model-a", "main")
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
		lazy,
		store,
		"/tmp/test",
		"main",
		"dev",
		nil,
	)

	updated, _ := model.handleSessionEvent(session.StatusChangedEvent{Status: "Thinking..."})
	model = updated

	if session.IsMaterialized(lazy) {
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
	sess := &stubSession{events: make(chan session.AgentEvent)}
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
	if len(storageSess.messages) != 0 {
		t.Fatalf("model-visible messages should not be app-persisted: %#v", storageSess.messages)
	}
}

func TestTokenUsageCancelsTurnWhenCostBudgetExceeded(t *testing.T) {
	sess := &stubSession{events: make(chan session.AgentEvent)}
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storageSess
	model.Model.Config = &config.Config{MaxTurnCost: 0.01}
	model.InFlight.Thinking = true
	model.Progress.Mode = stateStreaming
	model.Progress.LastToolUseID = "tool-1"
	model.InFlight.Pending = &session.Entry{Role: session.RoleAgent, Content: "partial"}
	model.InFlight.PendingTools = map[string]*session.Entry{
		"tool-1": {Role: session.RoleTool, Content: "partial tool"},
	}
	model.InFlight.Subagents = map[string]*SubagentProgress{
		"child-1": {ID: "child-1", Name: "child"},
	}
	model.InFlight.QueuedTurns = []string{"follow up"}
	model.InFlight.StreamBuf = "partial"
	model.InFlight.ReasonBuf = "reasoning"
	model.InFlight.AgentCommitted = true

	updated, cmd := model.handleSessionEvent(session.TokenUsageEvent{
		Input:  1000,
		Output: 100,
		Cost:   0.011,
	})
	model = updated

	if sess.cancels != 0 {
		t.Fatalf("cancels before command execution = %d, want 0", sess.cancels)
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
	if len(storageSess.appends) != 0 {
		t.Fatalf("appends before command execution = %#v, want none", storageSess.appends)
	}
	if cmd == nil {
		t.Fatal("expected cancellation command")
	}
	runSequencePrefix(t, cmd, 4)
	if sess.cancels != 1 {
		t.Fatalf("cancels after command execution = %d, want 1", sess.cancels)
	}
	var decision session.StoreRoutingDecision
	var system session.StoreSystem
	for _, event := range storageSess.appends {
		switch e := event.(type) {
		case session.StoreRoutingDecision:
			decision = e
		case session.StoreSystem:
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
	sess := &stubSession{events: make(chan session.AgentEvent)}
	storageSess := &stubStorageSession{appendErr: errors.New("disk full")}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storageSess
	model.Model.Config = &config.Config{MaxTurnCost: 0.01}
	model.InFlight.Thinking = true
	model.Progress.Mode = stateStreaming

	updated, cmd := model.handleSessionEvent(session.TokenUsageEvent{Cost: 0.011})
	model = updated

	if sess.cancels != 0 {
		t.Fatalf("cancels before command execution = %d, want 0", sess.cancels)
	}
	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want cancelled", model.Progress.Mode)
	}
	if cmd == nil {
		t.Fatal("expected cancellation command")
	}
	msgs := runSequencePrefix(t, cmd, 4)
	if sess.cancels != 1 {
		t.Fatalf("cancels after command execution = %d, want 1", sess.cancels)
	}
	var persistErrors int
	for _, msg := range msgs {
		if _, ok := msg.(localErrorMsg); ok {
			persistErrors++
		}
	}
	if persistErrors == 0 {
		t.Fatalf("command messages = %#v, want persistence error", msgs)
	}
}

func TestTurnFinishedPreservesBudgetCancellation(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateCancelled
	model.Progress.BudgetStopReason = "turn cost limit reached ($0.011000 / $0.010000)"
	model.InFlight.QueuedTurns = []string{"next turn"}

	updated, _ := model.Update(session.TurnFinishedEvent{})
	model = testModel(t, updated)

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

	updated, _ := model.Update(session.TurnFinishedEvent{})
	model = testModel(t, updated)

	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want stateCancelled", model.Progress.Mode)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want none", model.InFlight.QueuedTurns)
	}
}

func TestQueuedTurnRearmsEventReaderWhenSubmissionBlocked(t *testing.T) {
	sess := &stubSession{events: make(chan session.AgentEvent)}
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
	sess := &stubSession{events: make(chan session.AgentEvent)}
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
	sess := &stubSession{events: make(chan session.AgentEvent)}
	model := readyModel(t)
	model.Model.Session = sess
	model.InFlight.Thinking = true
	model.InFlight.Pending = &session.Entry{Role: session.RoleAgent, Content: "partial"}
	model.InFlight.StreamBuf = "partial"
	model.Progress.Status = "Running bash..."

	next, cmd := model.cancelRunningTurn("Canceled by user")
	model = next
	if cmd == nil {
		t.Fatal("expected cancellation print command")
	}
	if sess.cancels != 0 {
		t.Fatalf("cancels before command execution = %d, want 0", sess.cancels)
	}
	runCommandTree(t, cmd)
	if sess.cancels != 1 {
		t.Fatalf("cancels after command execution = %d, want 1", sess.cancels)
	}
	if !model.InFlight.DrainUntilTurnStarted {
		t.Fatal("expected cancel to drain late turn events")
	}
	if !model.InFlight.Thinking || !model.InFlight.Canceling {
		t.Fatalf(
			"cancel state = thinking %v canceling %v, want true/true",
			model.InFlight.Thinking,
			model.InFlight.Canceling,
		)
	}
	drainStartedAt := model.InFlight.DrainStartedAt
	if drainStartedAt.IsZero() {
		t.Fatal("expected cancel to record drain fence timestamp")
	}

	model.App.PrintedTranscript = false
	updated, _ := model.Update(session.UserMessageEvent{
		Base:    session.BaseAt(drainStartedAt.Add(-time.Millisecond)),
		Message: "stale canceled prompt",
	})
	model = testModel(t, updated)
	if model.App.PrintedTranscript {
		t.Fatal("late canceled-turn user message printed transcript output")
	}

	updated, _ = model.Update(session.TurnStartedEvent{
		Base: session.BaseAt(drainStartedAt.Add(-time.Millisecond)),
	})
	model = testModel(t, updated)
	if model.Progress.Mode != stateCancelled {
		t.Fatalf("late turn start reopened progress mode = %v", model.Progress.Mode)
	}
	if !model.InFlight.DrainUntilTurnStarted {
		t.Fatal("late turn start cleared drain fence")
	}
	for _, ev := range []session.AgentEvent{
		session.AgentDeltaEvent{Delta: "stale"},
		session.ThinkingDeltaEvent{Delta: "stale reasoning"},
		session.AgentMessageEvent{Message: "stale final"},
		session.ToolCallStartedEvent{ToolUseID: "tool-1", ToolName: "bash", Args: "echo stale"},
		session.ToolResultEvent{ToolUseID: "tool-1", ToolName: "bash", Result: "stale"},
		session.StatusChangedEvent{Status: "Ready"},
	} {
		updated, _ := model.Update(ev)
		model = testModel(t, updated)
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

	model.Input.Composer.SetValue("after cancel")
	updated, cmd = model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)
	if len(model.InFlight.QueuedTurns) != 1 || model.InFlight.QueuedTurns[0] != "after cancel" {
		t.Fatalf("queued turns = %#v, want [after cancel]", model.InFlight.QueuedTurns)
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared", got)
	}
	if cmd == nil {
		t.Fatal("expected queued follow-up notice while cancel is settling")
	}

	updated, cmd = model.Update(session.TurnFinishedEvent{})
	model = testModel(t, updated)
	if model.InFlight.DrainUntilTurnStarted {
		t.Fatal("cancel terminal did not clear drain fence")
	}
	if model.InFlight.Thinking || model.InFlight.Canceling {
		t.Fatalf(
			"cancel terminal state = thinking %v canceling %v, want false/false",
			model.InFlight.Thinking,
			model.InFlight.Canceling,
		)
	}
	if cmd == nil {
		t.Fatal("expected queued turn command after cancel terminal")
	}
	msg := cmd()
	queued, ok := msg.(queuedTurnMsg)
	if !ok {
		t.Fatalf("queued command returned %T, want queuedTurnMsg", msg)
	}
	if queued.text != "after cancel" || !queued.rearmSessionEvents {
		t.Fatalf("queued message = %#v, want rearmed after cancel", queued)
	}

	updated, cmd = model.Update(session.UserMessageEvent{
		Base:    session.BaseAt(drainStartedAt.Add(time.Millisecond)),
		Message: "fresh prompt",
	})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("fresh user message after cancel did not print")
	}

	updated, _ = model.Update(session.TurnStartedEvent{
		Base: session.BaseAt(drainStartedAt.Add(time.Millisecond)),
	})
	model = testModel(t, updated)
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

	updated, _ = model.Update(session.AgentDeltaEvent{Delta: "fresh"})
	model = testModel(t, updated)
	if model.InFlight.Pending == nil || model.InFlight.Pending.Content != "fresh" {
		t.Fatalf("fresh turn delta not accepted: %#v", model.InFlight.Pending)
	}
}

func TestCancelRunningTurnDoesNotWaitForStorageAppend(t *testing.T) {
	sess := &stubSession{events: make(chan session.AgentEvent)}
	storageSess := &blockingAppendStorage{
		entered: make(chan any, 1),
		release: make(chan struct{}),
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storageSess
	model.InFlight.Thinking = true

	next, cmd := model.cancelRunningTurn("Canceled by user")
	model = next

	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want cancelled", model.Progress.Mode)
	}
	select {
	case event := <-storageSess.entered:
		t.Fatalf("append ran during Update: %#v", event)
	default:
	}
	if len(storageSess.appends) != 0 {
		t.Fatalf("appends during Update = %#v, want none", storageSess.appends)
	}
	if sess.cancels != 0 {
		t.Fatalf("cancels before command execution = %d, want 0", sess.cancels)
	}

	children := commandChildren(t, cmd())
	done := make(chan []tea.Msg, len(children))
	for _, child := range children {
		go func() {
			done <- runCommandTree(t, child)
		}()
	}
	select {
	case event := <-storageSess.entered:
		system, ok := event.(session.StoreSystem)
		if !ok {
			t.Fatalf("append event = %#v, want cancellation system entry", event)
		}
		if system.Content != "Canceled by user" {
			t.Fatalf("system content = %q, want Canceled by user", system.Content)
		}
	case <-time.After(time.Second):
		t.Fatal("cancellation persistence command did not start append")
	}
	var sawCancel bool
	completed := 0
	for !sawCancel {
		select {
		case messages := <-done:
			completed++
			for _, msg := range messages {
				if _, ok := msg.(turnCancelResultMsg); ok {
					sawCancel = true
					break
				}
			}
		case <-time.After(time.Second):
			t.Fatal("cancel command waited for persistence to complete")
		}
	}
	if sess.cancels != 1 {
		t.Fatalf("cancels before persistence release = %d, want 1", sess.cancels)
	}
	close(storageSess.release)
	for completed < len(children) {
		select {
		case <-done:
			completed++
		case <-time.After(time.Second):
			t.Fatal("cancellation command batch did not finish")
		}
	}
	if len(storageSess.appends) != 1 {
		t.Fatalf(
			"appends after command execution = %#v, want one cancellation entry",
			storageSess.appends,
		)
	}
}

func TestCancelRunningTurnPersistenceFailureStillCancels(t *testing.T) {
	sess := &stubSession{events: make(chan session.AgentEvent)}
	storageSess := &stubStorageSession{appendErr: errors.New("disk full")}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storageSess
	model.InFlight.Thinking = true

	next, cmd := model.cancelRunningTurn("Canceled by user")
	model = next

	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want cancelled", model.Progress.Mode)
	}
	msgs := runSequencePrefix(t, cmd, 3)
	if sess.cancels != 1 {
		t.Fatalf("cancels after command execution = %d, want 1", sess.cancels)
	}
	var foundPersistError bool
	for _, msg := range msgs {
		if local, ok := msg.(localErrorMsg); ok &&
			strings.Contains(local.err.Error(), "persist cancellation: disk full") {
			foundPersistError = true
		}
	}
	if !foundPersistError {
		t.Fatalf("command messages = %#v, want cancellation persistence error", msgs)
	}
}

func TestCancelTurnCmdCallsBackend(t *testing.T) {
	sess := &stubSession{events: make(chan session.AgentEvent)}

	msg := cancelTurnCmd(sess)()
	result, ok := msg.(turnCancelResultMsg)
	if !ok {
		t.Fatalf("cancel command message = %T, want turnCancelResultMsg", msg)
	}
	if result.err != nil {
		t.Fatalf("cancel command error = %v", result.err)
	}
	if sess.cancels != 1 {
		t.Fatalf("cancels = %d, want 1", sess.cancels)
	}
}

func TestCancelTurnCmdReportsMissingSession(t *testing.T) {
	msg := cancelTurnCmd(nil)()
	result, ok := msg.(turnCancelResultMsg)
	if !ok {
		t.Fatalf("cancel command message = %T, want turnCancelResultMsg", msg)
	}
	if result.err == nil {
		t.Fatal("cancel command error = nil, want missing-session error")
	}
}

func TestTurnFinishedPreservesSessionError(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateError
	model.Progress.LastError = "prompt failed"
	model.InFlight.QueuedTurns = []string{"next turn"}

	updated, _ := model.Update(session.TurnFinishedEvent{})
	model = testModel(t, updated)

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
	sess := &stubSession{events: make(chan session.AgentEvent)}
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
	sess := &stubSession{events: make(chan session.AgentEvent)}
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
		stubBackend{sess: &stubSession{events: make(chan session.AgentEvent)}},
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
	sess := &stubSession{events: make(chan session.AgentEvent, 1)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Input.Composer.SetValue("follow up")
	model.InFlight.Thinking = true

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)
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
	updated, cmd = model.Update(session.TurnFinishedEvent{})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("expected queued turn command after finish")
	}
	msg := cmd()
	next, nextCmd := model.Update(msg)
	model = testModel(t, next)
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
	model = testModel(t, next)
	if nextCmd == nil {
		t.Fatal("expected queued submit result to re-arm session event wait")
	}
	eventResult := make(chan []tea.Msg, 1)
	go func() {
		eventResult <- runCommandTree(t, nextCmd)
	}()
	sess.events <- session.UserMessageEvent{Message: "follow up"}
	var eventMsg sessionEventMsg
	for _, msg := range <-eventResult {
		if ev, ok := msg.(sessionEventMsg); ok {
			eventMsg = ev
			break
		}
	}
	if eventMsg.event == nil {
		t.Fatal("queued follow-up command did not return sessionEventMsg")
	}
	if len(sess.submits) != 1 || sess.submits[0] != "follow up" {
		t.Fatalf("submits = %#v, want queued follow up", sess.submits)
	}
	if _, ok := eventMsg.event.(session.UserMessageEvent); !ok {
		t.Fatalf("queued follow-up event = %T, want UserMessage", eventMsg.event)
	}
	next, nextCmd = model.Update(eventMsg)
	model = testModel(t, next)
	if nextCmd == nil {
		t.Fatal("expected committed user message to print and re-arm session event wait")
	}
}

func TestBusyInputUsesBackendFollowUpQueueWhenAvailable(t *testing.T) {
	sess := &queuedInputStubSession{
		stubSession: stubSession{events: make(chan session.AgentEvent)},
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.Input.Composer.SetValue("follow up")
	model.InFlight.Thinking = true

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("expected backend follow-up command")
	}
	result, ok := cmd().(followUpResultMsg)
	if !ok {
		t.Fatalf("follow-up command returned %T, want followUpResultMsg", result)
	}
	if result.err != nil || result.result.Outcome != session.QueuedInputAccepted {
		t.Fatalf("follow-up result = %#v, want accepted", result)
	}
	updated, cmd = model.Update(result)
	model = testModel(t, updated)
	if len(sess.followUps) != 1 || sess.followUps[0] != "follow up" {
		t.Fatalf("followUps = %#v, want backend-owned follow-up", sess.followUps)
	}
	if len(model.InFlight.QueuedTurns) != 1 ||
		model.InFlight.QueuedTurns[0] != "follow up" ||
		!model.InFlight.QueuedTurnsBackendOwned {
		t.Fatalf(
			"queued projection = %#v owned=%v, want backend-owned follow up",
			model.InFlight.QueuedTurns,
			model.InFlight.QueuedTurnsBackendOwned,
		)
	}
	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want cleared after queueing", got)
	}
	if cmd == nil {
		t.Fatal("expected queue notice cmd")
	}

	updated, cmd = model.Update(session.TurnFinishedEvent{})
	model = testModel(t, updated)
	if len(sess.submits) != 0 {
		t.Fatalf("submits = %#v, want no local resubmit for backend-owned queue", sess.submits)
	}
	if cmd == nil {
		t.Fatal("expected normal finish command")
	}
}

func TestFollowUpResultPreservesBackendSteeringProjection(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.InFlight.QueuedSteering = []string{"steer already queued"}
	model.InFlight.QueuedTurns = []string{"follow up"}
	model.InFlight.QueuedTurnsBackendOwned = true

	updated, cmd := model.Update(followUpResultMsg{
		text:               "follow up",
		priorFollowUpCount: 0,
		result:             session.QueuedInputResult{Outcome: session.QueuedInputAccepted},
	})
	model = testModel(t, updated)

	if len(model.InFlight.QueuedSteering) != 1 ||
		model.InFlight.QueuedSteering[0] != "steer already queued" {
		t.Fatalf(
			"queued steering = %#v, want preserved backend steering",
			model.InFlight.QueuedSteering,
		)
	}
	if len(model.InFlight.QueuedTurns) != 1 || model.InFlight.QueuedTurns[0] != "follow up" {
		t.Fatalf("queued follow-ups = %#v, want no duplicate", model.InFlight.QueuedTurns)
	}
	if !model.InFlight.QueuedTurnsBackendOwned {
		t.Fatal("queued projection should remain backend-owned")
	}
	if cmd == nil {
		t.Fatal("expected queued follow-up notice")
	}
}

func TestQueuedInputUpdateOwnsBackendQueueProjection(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedSteering = []string{"local stale steer"}
	model.InFlight.QueuedTurns = []string{"local stale"}
	model.InFlight.QueuedTurnsBackendOwned = true

	updated, _ := model.Update(session.QueuedInputUpdatedEvent{
		Snapshot: session.QueuedInputSnapshot{
			Steering: []string{"backend steering"},
			FollowUp: []string{"backend follow-up"},
		},
	})
	model = testModel(t, updated)
	if len(model.InFlight.QueuedSteering) != 1 ||
		model.InFlight.QueuedSteering[0] != "backend steering" {
		t.Fatalf("steering projection = %#v, want backend snapshot", model.InFlight.QueuedSteering)
	}
	if len(model.InFlight.QueuedTurns) != 1 ||
		model.InFlight.QueuedTurns[0] != "backend follow-up" ||
		!model.InFlight.QueuedTurnsBackendOwned {
		t.Fatalf(
			"queued projection = %#v owned=%v, want backend snapshot",
			model.InFlight.QueuedTurns,
			model.InFlight.QueuedTurnsBackendOwned,
		)
	}

	updated, _ = model.Update(session.QueuedInputUpdatedEvent{})
	model = testModel(t, updated)
	if len(model.InFlight.QueuedSteering) != 0 ||
		len(model.InFlight.QueuedTurns) != 0 ||
		model.InFlight.QueuedTurnsBackendOwned {
		t.Fatalf(
			"queued projection = steer %#v follow %#v owned=%v, want cleared backend snapshot",
			model.InFlight.QueuedSteering,
			model.InFlight.QueuedTurns,
			model.InFlight.QueuedTurnsBackendOwned,
		)
	}
}

func TestBusyInputSteersDuringActiveToolWhenEnabled(t *testing.T) {
	sess := &steeringStubSession{
		stubSession: stubSession{events: make(chan session.AgentEvent)},
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Config = &config.Config{BusyInput: "steer"}
	model.Input.Composer.SetValue("use the smaller test")
	model.InFlight.Thinking = true
	model.InFlight.PendingTools = map[string]*session.Entry{
		"call-1": {Role: session.RoleTool, Title: "bash"},
	}

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)

	if cmd == nil {
		t.Fatal("expected steering command")
	}
	msg := cmd()
	result, ok := msg.(steeringResultMsg)
	if !ok {
		t.Fatalf("steering command returned %T, want steeringResultMsg", msg)
	}
	if result.err != nil || result.result.Outcome != session.SteeringAccepted {
		t.Fatalf("steering result = %#v, want accepted", result)
	}
	updated, cmd = model.Update(result)
	model = testModel(t, updated)
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

func TestBusyInputQueuesWhenSteeringDeclines(t *testing.T) {
	sess := &steeringStubSession{
		stubSession: stubSession{events: make(chan session.AgentEvent)},
		result:      session.SteeringResult{Outcome: session.SteeringQueued},
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Config = &config.Config{BusyInput: "steer"}
	model.Input.Composer.SetValue("queue instead")
	model.InFlight.Thinking = true
	model.InFlight.PendingTools = map[string]*session.Entry{
		"call-1": {Role: session.RoleTool, Title: "bash"},
	}

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)
	result, ok := cmd().(steeringResultMsg)
	if !ok {
		t.Fatalf("steering command returned unexpected result")
	}
	updated, cmd = model.Update(result)
	model = testModel(t, updated)

	if len(model.InFlight.QueuedTurns) != 1 ||
		model.InFlight.QueuedTurns[0] != "queue instead" {
		t.Fatalf("queued turns = %#v, want fallback queue", model.InFlight.QueuedTurns)
	}
	if cmd == nil {
		t.Fatal("expected queued follow-up notice command")
	}
}

func TestBusyInputSteersWithoutActiveToolBoundary(t *testing.T) {
	sess := &steeringStubSession{
		stubSession: stubSession{events: make(chan session.AgentEvent)},
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Config = &config.Config{BusyInput: "steer"}
	model.Input.Composer.SetValue("after this")
	model.InFlight.Thinking = true

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)
	if cmd == nil {
		t.Fatal("expected steering command")
	}
	result, ok := cmd().(steeringResultMsg)
	if !ok {
		t.Fatalf("steering command returned %T, want steeringResultMsg", result)
	}
	updated, _ = model.Update(result)
	model = testModel(t, updated)

	if len(sess.steers) != 1 || sess.steers[0] != "after this" {
		t.Fatalf("steers = %#v, want steering without active tools", sess.steers)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %#v, want none after steering", model.InFlight.QueuedTurns)
	}
}

func TestCtrlGRecallsBackendOwnedQueuedTurns(t *testing.T) {
	sess := &queuedInputStubSession{
		stubSession: stubSession{events: make(chan session.AgentEvent)},
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.InFlight.QueuedSteering = []string{"steer one"}
	model.InFlight.QueuedTurns = []string{"queued one", "queued two"}
	model.InFlight.QueuedTurnsBackendOwned = true
	model.Input.Composer.SetValue("draft")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'g', Mod: tea.ModCtrl})
	model = testModel(t, updated)
	if len(model.InFlight.QueuedSteering) != 0 ||
		len(model.InFlight.QueuedTurns) != 0 ||
		model.InFlight.QueuedTurnsBackendOwned {
		t.Fatalf(
			"queued turns = steer %#v follow %#v owned=%v, want cleared projection",
			model.InFlight.QueuedSteering,
			model.InFlight.QueuedTurns,
			model.InFlight.QueuedTurnsBackendOwned,
		)
	}
	if cmd == nil {
		t.Fatal("expected clear-and-set-draft command")
	}
	messages := runCommandTree(t, cmd)
	for _, msg := range messages {
		updated, _ = model.Update(msg)
		model = testModel(t, updated)
	}
	if sess.clears != 1 {
		t.Fatalf("clear calls = %d, want 1", sess.clears)
	}
	if got := model.Input.Composer.Value(); got != "draft\nsteer one\nqueued one\nqueued two" {
		t.Fatalf("composer = %q, want recalled queued turns", got)
	}
}

func TestQueuedSteeringRendersAboveComposer(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedSteering = []string{"use the smaller test"}
	model.InFlight.QueuedTurns = []string{"follow up after this"}
	model.InFlight.QueuedTurnsBackendOwned = true

	view := ansi.Strip(model.View().Content)
	for _, want := range []string{
		"Steering (Ctrl+G edit): use the smaller test",
		"+1 more",
		"2 queued",
	} {
		if !strings.Contains(view, want) {
			t.Fatalf("view missing %q:\n%s", want, view)
		}
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
	model = testModel(t, updated)

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
	model = testModel(t, updated)
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
	model = testModel(t, updated)

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
	sess := &stubSession{events: make(chan session.AgentEvent)}
	model := readyModel(t)
	model.Model.Session = sess
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("/definitely-not-a-command")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)

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
	sess := &stubSession{events: make(chan session.AgentEvent)}
	stored := &stubStorageSession{}
	model := New(stubBackend{sess: sess}, stored, nil, "/tmp/test", "main", "dev", nil)
	model.InFlight.Thinking = true
	model.InFlight.QueuedTurns = []string{"queued"}

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEscape})
	model = testModel(t, updated)

	if cmd == nil {
		t.Fatal("expected cancel command")
	}
	runCommandTree(t, cmd)
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
		events:    make(chan session.AgentEvent),
		submitErr: errors.New("backend unavailable"),
	}
	storeSess := &stubStorageSession{id: "stub-session"}
	model := readyModel(t)
	model.Model.Session = sess
	model.Model.Storage = storeSess
	model.Input.Composer.SetValue("hello")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)

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
	model = testModel(t, updated)
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
		if _, ok := event.(session.StoreRoutingDecision); ok {
			t.Fatalf(
				"immediate submit error persisted routing decision %#v; failed submissions should not materialize session state",
				event,
			)
		}
		if sys, ok := event.(session.StoreSystem); ok {
			t.Fatalf(
				"immediate submit error persisted system entry %#v; local errors should not materialize transcript state",
				sys,
			)
		}
	}
}

func TestSubmitTextClearsStaleErrorImmediately(t *testing.T) {
	sess := &stubSession{events: make(chan session.AgentEvent)}
	model := readyModel(t)
	model.Model.Session = sess
	model.Progress.Mode = stateError
	model.Progress.LastError = "old provider error"
	model.Progress.Status = "Running bash..."
	model.Input.Composer.SetValue("try again")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = testModel(t, updated)
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
