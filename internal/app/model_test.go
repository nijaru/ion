package app

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/nijaru/ion/internal/testutil"
)

type stubBackend struct {
	sess         *stubSession
	provider     string
	model        string
	providerSet  bool
	modelSet     bool
	contextLimit int
	surface      backend.ToolSurface
}

type compactBackend struct {
	stubBackend
	compacted bool
	err       error
	called    bool
}

type configCaptureBackend struct {
	stubBackend
	cfg *config.Config
}

func (b stubBackend) Name() string { return "stub" }
func (b stubBackend) Provider() string {
	if b.providerSet || b.provider != "" {
		return b.provider
	}
	return "stub"
}

func (b stubBackend) Model() string {
	if b.modelSet || b.model != "" {
		return b.model
	}
	return "stub-model"
}

func (b stubBackend) ContextLimit() int {
	if b.contextLimit != 0 {
		return b.contextLimit
	}
	return 0
}

func (b stubBackend) ToolSurface() backend.ToolSurface {
	if b.surface.Count != 0 ||
		b.surface.Sandbox != "" ||
		b.surface.Environment != "" ||
		len(b.surface.Names) > 0 {
		return b.surface
	}
	return backend.ToolSurface{
		Count:         2,
		LazyThreshold: 20,
		Names:         []string{"read", "write"},
	}
}

func (b stubBackend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []session.Entry{{Role: session.System, Content: "boot"}},
		Status:  "ready",
	}
}

func (b stubBackend) Session() session.AgentSession { return b.sess }

func (b stubBackend) SetStore(s storage.Store) {}

func (b stubBackend) SetSession(s storage.Session) {}

func (b stubBackend) SetConfig(cfg *config.Config) {}

func (b *configCaptureBackend) SetConfig(cfg *config.Config) {
	if cfg == nil {
		b.cfg = nil
		return
	}
	copied := *cfg
	b.cfg = &copied
}

func (b *compactBackend) Compact(ctx context.Context) (bool, error) {
	b.called = true
	return b.compacted, b.err
}

type stubSession struct {
	events      chan session.Event
	submits     []string
	cancels     int
	submitErr   error
	approveErr  error
	approvals   []stubApproval
	allowed     []string
	mode        session.Mode
	autoApprove bool
	closed      bool
}

type steeringStubSession struct {
	stubSession
	steers []string
	result session.SteeringResult
	err    error
}

type stubApproval struct {
	id string
	ok bool
}

func localErrorFromMsg(t *testing.T, msg tea.Msg) error {
	t.Helper()
	errMsg, ok := msg.(localErrorMsg)
	if !ok {
		t.Fatalf("message = %T, want localErrorMsg", msg)
	}
	return errMsg.err
}

func (s *stubSession) Open(ctx context.Context) error              { return nil }
func (s *stubSession) Resume(ctx context.Context, id string) error { return nil }
func (s *stubSession) SubmitTurn(ctx context.Context, turn string) error {
	if s.submitErr != nil {
		return s.submitErr
	}
	s.submits = append(s.submits, turn)
	return nil
}

func (s *stubSession) CancelTurn(ctx context.Context) error {
	s.cancels++
	return nil
}

func (s *stubSession) Close() error {
	s.closed = true
	if s.events != nil {
		close(s.events)
		s.events = nil
	}
	return nil
}
func (s *stubSession) Events() <-chan session.Event { return s.events }
func (s *stubSession) Approve(ctx context.Context, id string, ok bool) error {
	s.approvals = append(s.approvals, stubApproval{id: id, ok: ok})
	return s.approveErr
}

func (s *stubSession) SetMode(mode session.Mode) { s.mode = mode }

func (s *stubSession) SetAutoApprove(enabled bool) { s.autoApprove = enabled }
func (s *stubSession) AllowCategory(category string) {
	s.allowed = append(s.allowed, category)
}
func (s *stubSession) ID() string              { return "stub" }
func (s *stubSession) Meta() map[string]string { return nil }

func (s *steeringStubSession) SteerTurn(
	ctx context.Context,
	text string,
) (session.SteeringResult, error) {
	s.steers = append(s.steers, text)
	if s.err != nil {
		return session.SteeringResult{}, s.err
	}
	if s.result.Outcome == "" {
		return session.SteeringResult{Outcome: session.SteeringAccepted}, nil
	}
	return s.result, nil
}

type stubStorageSession struct {
	id         string
	model      string
	branch     string
	closed     bool
	appends    []any
	appendErr  error
	usageIn    int
	usageOut   int
	usageCost  float64
	entries    []session.Entry
	entriesErr error
}

func (s *stubStorageSession) ID() string { return s.id }

func (s *stubStorageSession) Meta() storage.Metadata {
	return storage.Metadata{
		ID:     s.id,
		Model:  s.model,
		Branch: s.branch,
	}
}

func (s *stubStorageSession) Append(ctx context.Context, event any) error {
	s.appends = append(s.appends, event)
	return s.appendErr
}

func (s *stubStorageSession) Entries(ctx context.Context) ([]session.Entry, error) {
	if s.entriesErr != nil {
		return nil, s.entriesErr
	}
	return append([]session.Entry(nil), s.entries...), nil
}

func (s *stubStorageSession) LastStatus(ctx context.Context) (string, error) { return "", nil }

func (s *stubStorageSession) Usage(ctx context.Context) (int, int, float64, error) {
	return s.usageIn, s.usageOut, s.usageCost, nil
}

func (s *stubStorageSession) Close() error {
	s.closed = true
	return nil
}

type resumeOnlyStore struct {
	resumed storage.Session
}

func (s *resumeOnlyStore) OpenSession(
	ctx context.Context,
	cwd, model, branch string,
) (storage.Session, error) {
	return nil, nil
}

func (s *resumeOnlyStore) ResumeSession(ctx context.Context, id string) (storage.Session, error) {
	return s.resumed, nil
}

func (s *resumeOnlyStore) ListSessions(
	ctx context.Context,
	cwd string,
) ([]storage.SessionInfo, error) {
	return nil, nil
}

func (s *resumeOnlyStore) GetRecentSession(
	ctx context.Context,
	cwd string,
) (*storage.SessionInfo, error) {
	return nil, nil
}

func (s *resumeOnlyStore) AddInput(ctx context.Context, cwd, content string) error { return nil }

func (s *resumeOnlyStore) GetInputs(ctx context.Context, cwd string, limit int) ([]string, error) {
	return nil, nil
}

func (s *resumeOnlyStore) UpdateSession(ctx context.Context, si storage.SessionInfo) error {
	return nil
}

func (s *resumeOnlyStore) Close() error { return nil }

type forkTreeStore struct {
	resumeOnlyStore
	forked     storage.Session
	forkParent string
	forkOpts   storage.ForkOptions
	tree       storage.SessionTree
}

func (s *forkTreeStore) ForkSession(
	ctx context.Context,
	parentID string,
	opts storage.ForkOptions,
) (storage.Session, error) {
	s.forkParent = parentID
	s.forkOpts = opts
	return s.forked, nil
}

func (s *forkTreeStore) SessionTree(
	ctx context.Context,
	sessionID string,
) (storage.SessionTree, error) {
	s.tree.Current.ID = sessionID
	return s.tree, nil
}

func readyModel(t *testing.T) Model {
	t.Helper()
	sess := &stubSession{events: make(chan session.Event)}
	b := stubBackend{sess: sess}
	model := New(b, nil, nil, "/tmp/test", "main", "dev", nil)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 30})
	ready, ok := updated.(Model)
	if !ok {
		t.Fatalf("expected Model after window size update")
	}
	return ready
}

func TestModelStreamsAndCommitsPendingEntry(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.AgentDelta{Delta: "streamed reply"})
	model = updated.(Model)

	if model.InFlight.Pending == nil || model.InFlight.Pending.Content != "streamed reply" {
		t.Fatalf("expected pending streamed agent entry, got %#v", model.InFlight.Pending)
	}

	updated, cmd := model.Update(session.AgentMessage{})
	model = updated.(Model)

	if model.InFlight.Pending != nil {
		t.Fatalf("expected pending entry to be cleared after flush")
	}

	// Verify that a Println command was returned
	if cmd == nil {
		t.Fatalf("expected tea.Println command after finalizing message")
	}
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.Agent); ok {
			t.Fatalf("agent message should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestPlaneBShowsPendingAgentText(t *testing.T) {
	model := readyModel(t)
	model.App.Width = 24
	model.InFlight.Pending = &session.Entry{
		Role:    session.Agent,
		Content: "streamed reply with a long tail",
	}

	got := ansi.Strip(model.renderPlaneB())
	if !strings.Contains(got, "• streamed reply with") ||
		!strings.Contains(got, "\n  long tail") {
		t.Fatalf("plane B = %q, want wrapped live assistant text", got)
	}
}

func TestPlaneBShowsPendingAgentTextWithoutMarkdownRendering(t *testing.T) {
	model := readyModel(t)
	model.App.Width = 80
	model.InFlight.Pending = &session.Entry{
		Role: session.Agent,
		Content: strings.Join([]string{
			"Working:",
			"",
			"```go",
			"fmt.Println(\"streaming\")",
		}, "\n"),
	}

	got := ansi.Strip(model.renderPlaneB())
	for _, want := range []string{
		"• Working:",
		"  ```go",
		"  fmt.Println(\"streaming\")",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("plane B = %q, want raw live markdown fragment %q", got, want)
		}
	}
}

func TestPlaneBTrimsLeadingNewlinesFromPendingAgentText(t *testing.T) {
	model := readyModel(t)
	model.App.Width = 80
	model.InFlight.Pending = &session.Entry{
		Role:    session.Agent,
		Content: "\n\n- first streamed bullet",
	}

	got := ansi.Strip(model.renderPlaneB())
	if strings.HasPrefix(got, "•\n") || strings.HasPrefix(got, "• \n") {
		t.Fatalf("plane B = %q, want no empty bullet row", got)
	}
	if !strings.Contains(got, "• - first streamed bullet") {
		t.Fatalf("plane B = %q, want leading markdown text on first row", got)
	}
}

func TestLateAgentDeltaAfterCommitIsIgnored(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.AgentDelta{Delta: "partial"})
	model = updated.(Model)
	updated, cmd := model.Update(session.AgentMessage{Message: "final"})
	model = updated.(Model)
	if cmd == nil {
		t.Fatal("expected committed assistant print command")
	}
	if !model.InFlight.AgentCommitted {
		t.Fatal("agent commit marker was not set")
	}

	updated, _ = model.Update(session.AgentDelta{Delta: "late"})
	model = updated.(Model)
	if model.InFlight.Pending != nil || model.InFlight.StreamBuf != "" {
		t.Fatalf(
			"late delta recreated pending stream: pending=%#v stream=%q",
			model.InFlight.Pending,
			model.InFlight.StreamBuf,
		)
	}

	updated, _ = model.Update(session.ThinkingDelta{Delta: "late thinking"})
	model = updated.(Model)
	if model.InFlight.ReasonBuf != "" {
		t.Fatalf("late thinking buffer = %q, want ignored", model.InFlight.ReasonBuf)
	}

	updated, _ = model.Update(session.TurnFinished{})
	model = updated.(Model)
	if model.InFlight.Pending != nil || model.InFlight.StreamBuf != "" ||
		model.InFlight.ReasonBuf != "" {
		t.Fatalf(
			"turn finish left pending stream: pending=%#v stream=%q reason=%q",
			model.InFlight.Pending,
			model.InFlight.StreamBuf,
			model.InFlight.ReasonBuf,
		)
	}
}

func TestToolEntryFlushesToTranscript(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess
	updated, _ := model.Update(session.ToolCallStarted{
		ToolUseID: "tool-call-1",
		ToolName:  "bash",
		Args:      "ls",
	})
	model = updated.(Model)

	if model.InFlight.Pending == nil || model.InFlight.Pending.Role != session.Tool {
		t.Fatalf("expected pending tool entry")
	}
	model.Progress.Status = "Running bash..."

	updated, cmd := model.Update(session.ToolResult{
		ToolName: "bash",
		Result:   "ok",
	})
	model = updated.(Model)

	if model.InFlight.Pending != nil {
		t.Fatalf("expected pending entry to be cleared")
	}
	if cmd == nil {
		t.Fatalf("expected tea.Println command for tool result")
	}
	if model.Progress.Mode != stateIonizing {
		t.Fatalf("progress mode = %v, want ionizing after tool completion", model.Progress.Mode)
	}
	if model.Progress.Status != "" {
		t.Fatalf("status = %q, want cleared after tool completion", model.Progress.Status)
	}
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.ToolResult); ok {
			t.Fatalf("tool result should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestAgentMessagePrintsWithoutPendingStream(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, cmd := model.Update(session.AgentMessage{Message: "done"})
	model = updated.(Model)

	if cmd == nil {
		t.Fatal("expected print command for committed assistant message")
	}
	if !model.App.PrintedTranscript {
		t.Fatal("committed assistant message did not mark transcript printed")
	}
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.Agent); ok {
			t.Fatalf("agent message should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestAgentMessageAfterToolResultPrintsFinalAnswer(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.ToolCallStarted{
		ToolUseID: "tool-call-1",
		ToolName:  "bash",
		Args:      "echo ok",
	})
	model = updated.(Model)
	updated, _ = model.Update(session.ToolResult{
		ToolUseID: "tool-call-1",
		ToolName:  "bash",
		Result:    "ok\n",
	})
	model = updated.(Model)

	model.App.PrintedTranscript = false
	updated, cmd := model.Update(session.AgentMessage{Message: "done"})
	model = updated.(Model)

	if cmd == nil {
		t.Fatal("expected print command for final assistant message after tool result")
	}
	if !model.App.PrintedTranscript {
		t.Fatal("final assistant message after tool result did not mark transcript printed")
	}
	if model.InFlight.Pending != nil {
		t.Fatalf("pending entry = %#v, want none", model.InFlight.Pending)
	}
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.Agent); ok {
			t.Fatalf("agent message should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestInterleavedToolResultsPreservePendingEntries(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, _ := model.Update(session.ToolCallStarted{
		ToolUseID: "tool-a",
		ToolName:  "bash",
		Args:      "first",
	})
	model = updated.(Model)

	updated, _ = model.Update(session.ToolCallStarted{
		ToolUseID: "tool-b",
		ToolName:  "bash",
		Args:      "second",
	})
	model = updated.(Model)

	updated, _ = model.Update(session.ToolOutputDelta{ToolUseID: "tool-a", Delta: "a partial"})
	model = updated.(Model)
	updated, _ = model.Update(session.ToolOutputDelta{ToolUseID: "tool-b", Delta: "b partial"})
	model = updated.(Model)

	if got := model.InFlight.PendingTools["tool-a"].Content; got != "a partial" {
		t.Fatalf("tool-a pending content = %q, want a partial", got)
	}
	if got := model.InFlight.PendingTools["tool-b"].Content; got != "b partial" {
		t.Fatalf("tool-b pending content = %q, want b partial", got)
	}

	updated, _ = model.Update(session.ToolResult{
		ToolUseID: "tool-a",
		ToolName:  "bash",
		Result:    "a done",
	})
	model = updated.(Model)

	if _, ok := model.InFlight.PendingTools["tool-a"]; ok {
		t.Fatal("tool-a pending entry still present after result")
	}
	if got := model.InFlight.PendingTools["tool-b"].Content; got != "b partial" {
		t.Fatalf("tool-b pending content = %q, want b partial", got)
	}

	updated, _ = model.Update(session.ToolResult{
		ToolUseID: "tool-b",
		ToolName:  "bash",
		Result:    "b done",
	})
	model = updated.(Model)

	if len(model.InFlight.PendingTools) != 0 {
		t.Fatalf("pending tools = %#v, want none", model.InFlight.PendingTools)
	}
	for _, event := range storageSess.appends {
		if _, ok := event.(storage.ToolResult); ok {
			t.Fatalf("tool results should not be app-persisted: %#v", storageSess.appends)
		}
	}
}

func TestUnknownToolResultIDDoesNotClearAnotherPendingTool(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	updated, _ := model.Update(session.ToolCallStarted{
		ToolUseID: "tool-a",
		ToolName:  "bash",
		Args:      "first",
	})
	model = updated.(Model)

	updated, _ = model.Update(session.ToolResult{
		ToolUseID: "missing-tool",
		ToolName:  "bash",
		Result:    "wrong result",
	})
	model = updated.(Model)

	if _, ok := model.InFlight.PendingTools["tool-a"]; !ok {
		t.Fatal("known pending tool was cleared by unknown tool result")
	}
	for _, event := range storageSess.appends {
		if result, ok := event.(storage.ToolResult); ok && result.ToolUseID == "missing-tool" {
			t.Fatal("unknown tool result was persisted")
		}
	}
}

func TestTurnFinishedLeavesProgressComplete(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming
	model.InFlight.Thinking = true
	model.InFlight.AgentCommitted = true
	model.Progress.TurnStartedAt = time.Now().Add(-3 * time.Second)
	model.Progress.CurrentTurnInput = 1200
	model.Progress.CurrentTurnOutput = 300

	updated, _ := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress = %v, want stateComplete", model.Progress.Mode)
	}
	line := ansi.Strip(model.progressLine())
	if !strings.Contains(line, "✓ Complete") {
		t.Fatalf("progress line = %q, want complete state", line)
	}
	for _, want := range []string{"↑ 1.2k", "↓ 300", "3s"} {
		if !strings.Contains(line, want) {
			t.Fatalf("progress line = %q, missing %q", line, want)
		}
	}
	if strings.Index(line, "3s") < strings.Index(line, "↓ 300") {
		t.Fatalf("progress line = %q, want elapsed time after token counters", line)
	}
}

func TestTurnFinishedCommitsPendingStreamWhenNoAgentMessageArrives(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateStreaming
	model.InFlight.Pending = &session.Entry{Role: session.Agent, Content: "streamed answer"}
	model.InFlight.StreamBuf = "streamed answer"
	model.InFlight.ReasonBuf = "brief reasoning"
	model.InFlight.Thinking = true

	updated, cmd := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.InFlight.Pending != nil {
		t.Fatalf("pending agent entry = %#v, want flushed", model.InFlight.Pending)
	}
	if model.InFlight.StreamBuf != "" || model.InFlight.ReasonBuf != "" {
		t.Fatalf(
			"stream buffers = %q/%q, want cleared",
			model.InFlight.StreamBuf,
			model.InFlight.ReasonBuf,
		)
	}
	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress = %v, want complete", model.Progress.Mode)
	}
	if cmd == nil {
		t.Fatal("expected print command for flushed pending stream")
	}
}

func TestTurnFinishedWithoutAssistantResponseShowsError(t *testing.T) {
	model := readyModel(t)
	model.Progress.Mode = stateWorking
	model.Progress.TurnStartedAt = time.Now().Add(-2 * time.Second)
	model.InFlight.Pending = &session.Entry{Role: session.Agent}
	model.InFlight.QueuedTurns = []string{"follow-up"}
	model.InFlight.Thinking = true

	updated, cmd := model.Update(session.TurnFinished{})
	model = updated.(Model)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "turn finished without assistant response" {
		t.Fatalf("last error = %q", model.Progress.LastError)
	}
	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %#v, want cleared", model.InFlight.QueuedTurns)
	}
	if cmd == nil {
		t.Fatal("expected command to print visible error")
	}
}

func TestNewRestoresActivePresetFromState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	if err := config.SaveActivePreset("fast"); err != nil {
		t.Fatalf("save active preset: %v", err)
	}

	model := readyModel(t)
	if model.App.ActivePreset != presetFast {
		t.Fatalf("active preset = %q, want fast", model.App.ActivePreset)
	}
}

func TestWithConfigForRuntimeKeepsAppConfigAndAppliesRuntimeConfig(t *testing.T) {
	sess := &stubSession{events: make(chan session.Event)}
	capture := &configCaptureBackend{stubBackend: stubBackend{sess: sess}}
	model := New(capture, nil, nil, "/tmp/test", "main", "dev", nil).
		WithConfigForRuntime(
			&config.Config{
				Provider:            "openai",
				Model:               "gpt-4.1",
				ReasoningEffort:     "high",
				FastModel:           "gpt-4.1-mini",
				FastReasoningEffort: "low",
			},
			&config.Config{
				Provider:        "openai",
				Model:           "gpt-4.1-mini",
				ReasoningEffort: "low",
			},
		).
		WithActivePreset("fast")

	if model.App.ActivePreset != presetFast {
		t.Fatalf("active preset = %q, want fast", model.App.ActivePreset)
	}
	if model.Model.Config == nil || model.Model.Config.Model != "gpt-4.1" ||
		model.Model.Config.FastModel != "gpt-4.1-mini" {
		t.Fatalf("app config = %#v, want full preset config", model.Model.Config)
	}
	if capture.cfg == nil || capture.cfg.Model != "gpt-4.1-mini" ||
		capture.cfg.ReasoningEffort != "low" {
		t.Fatalf("backend cfg = %#v, want resolved fast runtime config", capture.cfg)
	}
	if model.Progress.ReasoningEffort != "low" {
		t.Fatalf("progress reasoning = %q, want low", model.Progress.ReasoningEffort)
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

func TestChildLifecycleUpdatesPlaneB(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.handleSessionEvent(session.ChildRequested{
		AgentName: "worker-1",
		Query:     "inspect the repo",
	})
	model = updated
	if model.InFlight.Subagents["worker-1"] == nil ||
		model.InFlight.Subagents["worker-1"].Name != "worker-1" {
		t.Fatalf(
			"pending child after request = %#v, want subagent progress in Subagents map",
			model.InFlight.Subagents["worker-1"],
		)
	}
	if model.InFlight.Subagents["worker-1"].Name != "worker-1" {
		t.Fatalf("child name = %q, want worker-1", model.InFlight.Subagents["worker-1"].Name)
	}
	if model.InFlight.Subagents["worker-1"].Intent != "inspect the repo" {
		t.Fatalf("child intent = %q, want query", model.InFlight.Subagents["worker-1"].Intent)
	}

	updated, _ = model.handleSessionEvent(session.ChildStarted{
		AgentName: "worker-1",
	})
	model = updated
	if model.InFlight.Subagents["worker-1"] == nil ||
		model.InFlight.Subagents["worker-1"].Status != "Started" {
		t.Fatalf(
			"child status after start = %q, want Started",
			model.InFlight.Subagents["worker-1"].Status,
		)
	}

	updated, _ = model.handleSessionEvent(session.ChildDelta{
		AgentName: "worker-1",
		Delta:     "thinking...\n",
	})
	model = updated
	if model.InFlight.Subagents["worker-1"] == nil ||
		!strings.Contains(model.InFlight.Subagents["worker-1"].Output, "thinking...") {
		t.Fatalf(
			"child output after delta = %#v, want streamed delta",
			model.InFlight.Subagents["worker-1"],
		)
	}

	updated, _ = model.handleSessionEvent(session.ChildCompleted{
		AgentName: "worker-1",
		Result:    "done",
	})
	model = updated
	if model.InFlight.Subagents["worker-1"] != nil {
		t.Fatalf("expected child entry to clear, got %#v", model.InFlight.Subagents["worker-1"])
	}
	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress mode after child complete = %v, want stateComplete", model.Progress.Mode)
	}

	updated, _ = model.handleSessionEvent(session.ChildRequested{
		AgentName: "worker-2",
		Query:     "recover from failure",
	})
	model = updated

	updated, _ = model.handleSessionEvent(session.ChildFailed{
		AgentName: "worker-2",
		Error:     "boom",
	})
	model = updated
	if model.InFlight.Subagents["worker-2"] != nil {
		t.Fatalf(
			"expected failed child entry to clear, got %#v",
			model.InFlight.Subagents["worker-2"],
		)
	}
	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode after child failure = %v, want stateError", model.Progress.Mode)
	}
	if model.Progress.LastError != "Subagent failed: boom" {
		t.Fatalf(
			"last error after child failure = %q, want subagent error",
			model.Progress.LastError,
		)
	}
}

func TestChildBlockedUpdatesPlaneB(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.handleSessionEvent(session.ChildRequested{
		AgentName: "worker-3",
		Query:     "wait for approval",
	})
	model = updated

	updated, _ = model.handleSessionEvent(session.ChildBlocked{
		AgentName: "worker-3",
		Reason:    "needs approval",
	})
	model = updated

	if model.InFlight.Subagents["worker-3"] == nil ||
		model.InFlight.Subagents["worker-3"].Name != "worker-3" {
		t.Fatalf(
			"pending child after block = %#v, want subagent progress in Subagents map",
			model.InFlight.Subagents["worker-3"],
		)
	}
	if got := model.InFlight.Subagents["worker-3"].Output; !strings.Contains(
		got,
		"BLOCKED: needs approval",
	) {
		t.Fatalf("child output = %q, want blocked notice", got)
	}
	if model.Progress.Mode != stateBlocked {
		t.Fatalf("progress mode = %v, want stateBlocked", model.Progress.Mode)
	}
	if model.InFlight.Thinking {
		t.Fatal("blocked child should stop the active thinking spinner")
	}
	if got := ansi.Strip(model.progressLine()); !strings.Contains(got, "Subagent blocked") {
		t.Fatalf("progress line = %q, want blocked state", got)
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

func TestPickerCommitSwitchesRuntime(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte("provider = \"openai\"\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.Event)}
	oldBackend := stubBackend{sess: oldSession}

	switched := false
	observedSessionID := ""
	model := New(
		oldBackend,
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			switched = true
			observedSessionID = sessionID

			resolved := *cfg
			resolved.Provider = "openai"

			newStorage := &stubStorageSession{
				id:     sessionID,
				model:  resolved.Model,
				branch: "feature/switch",
			}

			newBackend := testutil.New()
			newBackend.SetConfig(&resolved)
			newBackend.SetSession(newStorage)

			return newBackend, newBackend.Session(), newStorage, nil
		},
	)

	model.Picker.Overlay = &pickerOverlayState{
		title:   "Pick a model for openai",
		items:   []pickerItem{{Label: "gpt-4.1", Value: "gpt-4.1"}},
		index:   0,
		purpose: pickerPurposeModel,
		cfg:     &config.Config{Provider: "openai"},
	}

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated
	msg := cmd()

	switchedMsg, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}

	next, _ := model.Update(switchedMsg)
	model = next.(Model)

	if !switched {
		t.Fatal("expected runtime switch callback to be invoked")
	}
	if observedSessionID != oldSession.ID() {
		t.Fatalf("session ID passed to switcher = %q, want %q", observedSessionID, oldSession.ID())
	}
	if got := model.Model.Backend.Provider(); got != "openai" {
		t.Fatalf("backend provider = %q, want %q", got, "openai")
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1" {
		t.Fatalf("backend model = %q, want %q", got, "gpt-4.1")
	}
	if got := model.Model.Session.ID(); got != oldSession.ID() {
		t.Fatalf("session ID = %q, want %q", got, oldSession.ID())
	}
	if got := model.Model.Storage.ID(); got != oldSession.ID() {
		t.Fatalf("storage session ID = %q, want %q", got, oldSession.ID())
	}
	if got := model.App.Branch; got != "feature/switch" {
		t.Fatalf("branch = %q, want %q", got, "feature/switch")
	}
}

func TestPickerCommitSameModelIsNoOp(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "z-ai/glm-5",
	}
	model.Picker.Overlay = &pickerOverlayState{
		title:   "Pick a model for openrouter",
		items:   []pickerItem{{Label: "z-ai/glm-5", Value: "z-ai/glm-5"}},
		index:   0,
		purpose: pickerPurposeModel,
		cfg:     &config.Config{Provider: "openrouter", Model: "z-ai/glm-5"},
	}

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated

	if cmd != nil {
		t.Fatalf("expected no command when selecting the active model, got %T", cmd)
	}
	if model.Picker.Overlay != nil {
		t.Fatal("expected picker to close on same-model selection")
	}
	if got := model.Model.Backend.Model(); got != "z-ai/glm-5" {
		t.Fatalf("backend model = %q, want z-ai/glm-5", got)
	}
}

func TestProviderPickerSelectingCurrentProviderOpensModelPickerWithoutClearingModel(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "z-ai/glm-5",
	}
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		if cfg.Provider != "openrouter" {
			t.Fatalf("provider = %q, want openrouter", cfg.Provider)
		}
		return []registry.ModelMetadata{
			{ID: "z-ai/glm-4.5"},
			{ID: "z-ai/glm-5"},
			{ID: "z-ai/glm-5-turbo"},
		}, nil
	}
	defer func() { listModelsForConfig = oldListModelsForConfig }()

	model.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a provider",
		items:    providerItems(&config.Config{}),
		filtered: providerItems(&config.Config{}),
		index:    pickerIndex(providerItems(&config.Config{}), "openrouter"),
		purpose:  pickerPurposeProvider,
		cfg:      &config.Config{Provider: "openrouter", Model: "z-ai/glm-5"},
	}

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated
	if cmd != nil {
		t.Fatalf("expected no command when reopening model picker, got %T", cmd)
	}
	if model.Picker.Overlay == nil {
		t.Fatal("expected model picker to open")
	}
	if model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker purpose = %v, want model picker", model.Picker.Overlay.purpose)
	}
	if model.Picker.Overlay.cfg == nil {
		t.Fatal("expected picker config to be preserved")
	}
	if got := model.Picker.Overlay.cfg.Provider; got != "openrouter" {
		t.Fatalf("picker provider = %q, want openrouter", got)
	}
	if got := model.Picker.Overlay.cfg.Model; got != "z-ai/glm-5" {
		t.Fatalf("picker model = %q, want z-ai/glm-5", got)
	}
	if got := pickerDisplayItems(model.Picker.Overlay)[model.Picker.Overlay.index].Value; got != "z-ai/glm-5" {
		t.Fatalf("selected model = %q, want z-ai/glm-5", got)
	}
	if got := model.Model.Backend.Provider(); got != "openrouter" {
		t.Fatalf("backend provider = %q, want openrouter", got)
	}
	if got := model.Model.Backend.Model(); got != "z-ai/glm-5" {
		t.Fatalf("backend model = %q, want z-ai/glm-5", got)
	}
}

func TestModelPickerRejectsProviderWithoutModelListing(t *testing.T) {
	model := readyModel(t)

	updated, cmd := model.openModelPickerWithConfig(&config.Config{Provider: "zai"})
	model = updated
	if model.Picker.Overlay != nil {
		t.Fatal("expected no model picker for provider without listing support")
	}
	if cmd == nil {
		t.Fatal("expected model picker error command")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "Set a model with /model <id>") {
		t.Fatalf("error = %v, want manual model entry notice", err)
	}
}

func TestProviderPickerSelectingNonListingProviderClearsStaleError(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	model := readyModel(t)
	model.Progress.Mode = stateError
	model.Progress.LastError = "failed to list models for zai"
	model.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a provider",
		items:    providerItems(&config.Config{}),
		filtered: providerItems(&config.Config{}),
		index:    pickerIndex(providerItems(&config.Config{}), "zai"),
		purpose:  pickerPurposeProvider,
		cfg:      &config.Config{Provider: "openrouter", Model: "vendor/model-b"},
	}

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated
	if cmd == nil {
		t.Fatal("expected non-listing provider selection notice")
	}
	if model.Progress.Mode == stateError || model.Progress.LastError != "" {
		t.Fatalf(
			"stale error not cleared: mode=%v err=%q",
			model.Progress.Mode,
			model.Progress.LastError,
		)
	}
	cfg, err := config.Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.Provider != "zai" {
		t.Fatalf("config provider = %q, want zai", cfg.Provider)
	}
	if cfg.Model != "" {
		t.Fatalf("config model = %q, want cleared model", cfg.Model)
	}
}

func TestProviderCommandClearsStaleError(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		if cfg.Provider != "anthropic" {
			t.Fatalf("provider = %q, want anthropic", cfg.Provider)
		}
		return []registry.ModelMetadata{{ID: "claude-test"}}, nil
	}
	t.Cleanup(func() { listModelsForConfig = oldListModelsForConfig })

	model := readyModel(t)
	model.Progress.Mode = stateError
	model.Progress.LastError = "failed to list models for zai"

	updated, cmd := model.handleCommand("/provider anthropic")
	model = updated

	if cmd != nil {
		t.Fatalf("expected provider command to open picker without command, got %T", cmd)
	}
	if model.Progress.Mode == stateError || model.Progress.LastError != "" {
		t.Fatalf(
			"stale error not cleared: mode=%v err=%q",
			model.Progress.Mode,
			model.Progress.LastError,
		)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker = %#v, want model picker", model.Picker.Overlay)
	}
}

func TestRuntimeSwitchKeepsNoticesOutOfTranscriptStorage(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte("provider = \"openai\"\nmodel = \"gpt-4.1\"\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.Event)}
	oldBackend := stubBackend{sess: oldSession}

	newStorage := &stubStorageSession{
		id:     oldSession.ID(),
		model:  "openai/gpt-4.1",
		branch: "feature/switch",
	}
	model := New(
		oldBackend,
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			resolved := *cfg
			newBackend := testutil.New()
			newBackend.SetConfig(&resolved)
			newBackend.SetSession(newStorage)
			return newBackend, newBackend.Session(), newStorage, nil
		},
	)

	next, _ := model.Update(runtimeSwitchedMsg{
		backend: testutil.New(),
		session: testutil.New(),
		storage: newStorage,
		status:  "ready",
		notice:  "Switched model to gpt-4.1",
	})
	model = next.(Model)

	if len(newStorage.appends) != 0 {
		t.Fatalf(
			"expected runtime switch notice to stay out of transcript storage, got %d appends",
			len(newStorage.appends),
		)
	}
}

func TestRuntimeSwitchClearsQueuedTurns(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedTurns = []string{"stale follow up"}
	model.Progress.LastError = "old error"
	model.Progress.LastTurnSummary = turnSummary{Elapsed: time.Second, Input: 1, Output: 2, Cost: 3}

	next, _ := model.Update(runtimeSwitchedMsg{
		backend: stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		session: &stubSession{events: make(chan session.Event)},
		storage: &stubStorageSession{id: "session-1", branch: "main"},
		status:  "ready",
	})
	model = next.(Model)

	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want cleared on runtime switch", model.InFlight.QueuedTurns)
	}
	if model.Progress.LastError != "" {
		t.Fatalf("last error = %q, want cleared on runtime switch", model.Progress.LastError)
	}
	if model.Progress.LastTurnSummary != (turnSummary{}) {
		t.Fatalf(
			"last turn summary = %#v, want cleared on runtime switch",
			model.Progress.LastTurnSummary,
		)
	}
}

func TestRuntimeSwitchClosesNewRuntimeWhenStateSaveFails(t *testing.T) {
	t.Setenv("HOME", "/dev/null")
	oldSession := &stubSession{events: make(chan session.Event)}
	newSession := &stubSession{events: make(chan session.Event)}
	newStorage := &stubStorageSession{id: "new-session", branch: "main"}
	model := New(
		stubBackend{sess: oldSession},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			return stubBackend{sess: newSession}, newSession, newStorage, nil
		},
	)

	cmd := model.switchRuntimeCommand(
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		presetFast,
		session.Entry{Role: session.System, Content: "Switched"},
		"",
		false,
	)
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(err.Error(), "save active preset") {
		t.Fatalf("switch error = %v, want save active preset error", err)
	}
	if oldSession.closed {
		t.Fatal("old session was closed after failed switch")
	}
	if !newSession.closed {
		t.Fatal("new session was not closed after failed switch")
	}
	if !newStorage.closed {
		t.Fatal("new storage was not closed after failed switch")
	}
}

func TestSessionErrorClearsQueuedTurnsAndSetsError(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedTurns = []string{"stale follow up"}
	model.Progress.LastError = "old error"

	next, _ := model.Update(session.Error{Err: errors.New("backend failed")})
	model = next.(Model)

	if len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("queued turns = %v, want cleared on session error", model.InFlight.QueuedTurns)
	}
	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "backend failed" {
		t.Fatalf("last error = %q, want backend failed", model.Progress.LastError)
	}
}

func TestLocalErrorPrintsWithoutProgressError(t *testing.T) {
	model := readyModel(t)

	next, cmd := model.Update(localErrorMsg{err: errors.New("unknown command")})
	model = next.(Model)

	if cmd == nil {
		t.Fatal("expected local error print command")
	}
	if model.Progress.Mode == stateError || model.Progress.LastError != "" {
		t.Fatalf(
			"progress after local error = %v/%q, want no live error",
			model.Progress.Mode,
			model.Progress.LastError,
		)
	}
}

func TestSessionErrorClassifiesProviderRateLimit(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	err := errors.New("error, status code: 429 Too Many Requests: rate limit exceeded")
	next, _ := model.Update(session.Error{Err: err})
	model = next.(Model)

	if !strings.HasPrefix(model.Progress.LastError, "API rate limit: ") {
		t.Fatalf("last error = %q, want API rate limit prefix", model.Progress.LastError)
	}
	if !strings.Contains(model.Progress.LastError, err.Error()) {
		t.Fatalf("last error = %q, want raw provider error", model.Progress.LastError)
	}
	var decision storage.RoutingDecision
	var sys storage.System
	for _, event := range storageSess.appends {
		switch e := event.(type) {
		case storage.RoutingDecision:
			decision = e
		case storage.System:
			sys = e
		}
	}
	if decision.Decision != "stop" || decision.Reason != "rate_limit" {
		t.Fatalf("routing decision = %#v, want stop/rate_limit", decision)
	}
	if decision.StopReason != err.Error() {
		t.Fatalf("stop reason = %q, want raw provider error", decision.StopReason)
	}
	if !strings.Contains(sys.Content, "API rate limit: "+err.Error()) {
		t.Fatalf("system error = %q, want classified raw error", sys.Content)
	}
}

func TestSessionErrorClassifiesProviderQuotaLimit(t *testing.T) {
	storageSess := &stubStorageSession{}
	model := readyModel(t)
	model.Model.Storage = storageSess

	err := errors.New("insufficient_quota: billing hard limit has been reached")
	next, _ := model.Update(session.Error{Err: err})
	model = next.(Model)

	if !strings.HasPrefix(model.Progress.LastError, "API quota or usage limit: ") {
		t.Fatalf("last error = %q, want quota limit prefix", model.Progress.LastError)
	}
	var decision storage.RoutingDecision
	for _, event := range storageSess.appends {
		if e, ok := event.(storage.RoutingDecision); ok {
			decision = e
			break
		}
	}
	if decision.Decision != "stop" || decision.Reason != "quota_limit" {
		t.Fatalf("routing decision = %#v, want stop/quota_limit", decision)
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

func TestTurnStartedClearsStaleToolStatus(t *testing.T) {
	model := readyModel(t)
	model.Progress.Status = "Running bash..."

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)

	if model.Progress.Status != "" {
		t.Fatalf("status = %q, want cleared", model.Progress.Status)
	}
}

func TestSlashModelSameValueIsNoOp(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte("provider = \"openrouter\"\nmodel = \"z-ai/glm-5\"\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:     &stubSession{events: make(chan session.Event)},
		provider: "openrouter",
		model:    "z-ai/glm-5",
	}

	model, cmd := model.handleCommand("/model z-ai/glm-5")
	if cmd != nil {
		t.Fatalf("expected no-op command for same model, got %T", cmd)
	}
}

func TestSlashModelUsesRuntimeConfigOverPersistedState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(cfgDir, "state.toml"),
		[]byte("provider = \"local-api\"\nmodel = \"qwen3.6:27b\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write state: %v", err)
	}

	oldSession := &stubSession{events: make(chan session.Event)}
	var observed *config.Config
	model := New(
		stubBackend{sess: oldSession, provider: "openrouter", model: "tencent/hy3-preview:free"},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			copied := *cfg
			observed = &copied
			newBackend := testutil.New()
			newBackend.SetConfig(&copied)
			return newBackend, newBackend.Session(), nil, nil
		},
	).WithConfig(&config.Config{
		Provider: "openrouter",
		Model:    "tencent/hy3-preview:free",
	})

	model, cmd := model.handleCommand("/model anthropic/claude-sonnet-4.5")
	if cmd == nil {
		t.Fatal("expected runtime switch command")
	}
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	next, _ := model.Update(switched)
	model = next.(Model)

	if observed == nil || observed.Provider != "openrouter" ||
		observed.Model != "anthropic/claude-sonnet-4.5" {
		t.Fatalf("switcher config = %#v, want active openrouter provider", observed)
	}
	if model.Model.Config == nil ||
		model.Model.Config.Provider != "openrouter" ||
		model.Model.Config.Model != "anthropic/claude-sonnet-4.5" {
		t.Fatalf("app config = %#v, want updated openrouter model", model.Model.Config)
	}
	state, err := config.LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.Provider == nil || *state.Provider != "openrouter" ||
		state.Model == nil || *state.Model != "anthropic/claude-sonnet-4.5" {
		t.Fatalf("state = %#v, want explicit slash command selection persisted", state)
	}
}

func TestRuntimeSwitchShowsStatusOnResume(t *testing.T) {
	model := readyModel(t)
	model.Model.Session = &stubSession{events: make(chan session.Event)}

	updated, cmd := model.Update(runtimeSwitchedMsg{
		backend:       stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		session:       &stubSession{events: make(chan session.Event)},
		storage:       &stubStorageSession{id: "session-1", branch: "main"},
		printLines:    []string{"ion v0.0.0", "~/tmp/test • main", "", "--- resumed ---"},
		replayEntries: []session.Entry{{Role: session.User, Content: "hello"}},
		status:        "Connected via Canto",
		notice:        "Resumed session session-1",
		showStatus:    false,
	})
	model = updated.(Model)

	if model.Progress.Status != "Connected via Canto" {
		t.Fatalf("status = %q", model.Progress.Status)
	}
	if cmd == nil {
		t.Fatal("expected command batch for runtime switch")
	}
}

func TestResumeStoredSessionClosesInspectionSession(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(cfgDir, "config.toml"), []byte("provider = \"openai\"\nmodel = \"gpt-4.1\"\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	tempSession := &stubStorageSession{
		id:     "session-1",
		model:  "openai/gpt-4.1",
		branch: "main",
	}

	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		nil,
		&resumeOnlyStore{resumed: tempSession},
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			newBackend := testutil.New()
			opened := &stubStorageSession{
				id:     sessionID,
				model:  cfg.Provider + "/" + cfg.Model,
				branch: "feature/resume",
			}
			newBackend.SetConfig(cfg)
			newBackend.SetSession(opened)
			return newBackend, newBackend.Session(), opened, nil
		},
	)

	cmd := model.resumeStoredSessionByID("session-1")
	msg := cmd()

	if _, ok := msg.(runtimeSwitchedMsg); !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}
	if !tempSession.closed {
		t.Fatal("expected temporary inspection session to be closed after reading metadata")
	}
}

func TestResumeRuntimeCommandPrintsMarkerAfterHeader(t *testing.T) {
	newSession := &stubSession{events: make(chan session.Event)}
	newStorage := &stubStorageSession{
		id:      "session-1",
		model:   "openai/gpt-4.1",
		branch:  "feature/resume",
		entries: []session.Entry{{Role: session.User, Content: "hello"}},
	}
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			return stubBackend{sess: newSession}, newSession, newStorage, nil
		},
	)

	cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		session.Entry{Role: session.System, Content: "Resumed"},
		"session-1",
	)
	msg := cmd()
	switched, ok := msg.(runtimeSwitchedMsg)
	if !ok {
		t.Fatalf("expected runtimeSwitchedMsg, got %T", msg)
	}

	got := make([]string, 0, len(switched.printLines))
	for _, line := range switched.printLines {
		got = append(got, ansi.Strip(line))
	}
	want := []string{"ion dev", "/tmp/test • feature/resume", "", "--- resumed ---", ""}
	if !slices.Equal(got, want) {
		t.Fatalf("print lines = %#v, want %#v", got, want)
	}
	if len(switched.replayEntries) != 1 || switched.replayEntries[0].Content != "hello" {
		t.Fatalf("replay entries = %#v", switched.replayEntries)
	}
}

func TestResumeRuntimeCommandClosesNewRuntimeWhenReplayFails(t *testing.T) {
	oldSession := &stubSession{events: make(chan session.Event)}
	newSession := &stubSession{events: make(chan session.Event)}
	newStorage := &stubStorageSession{
		id:         "session-1",
		model:      "openai/gpt-4.1",
		branch:     "main",
		entriesErr: errors.New("bad replay"),
	}
	model := New(
		stubBackend{sess: oldSession},
		nil,
		nil,
		"/tmp/test",
		"main",
		"dev",
		func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
			return stubBackend{sess: newSession}, newSession, newStorage, nil
		},
	)

	cmd := model.resumeRuntimeCommand(
		&config.Config{Provider: "openai", Model: "gpt-4.1"},
		session.Entry{Role: session.System, Content: "Resumed"},
		"session-1",
	)
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(
		err.Error(),
		"load session transcript",
	) {
		t.Fatalf("resume error = %v, want transcript load error", err)
	}
	if oldSession.closed {
		t.Fatal("old session was closed after failed resume")
	}
	if !newSession.closed {
		t.Fatal("new session was not closed after failed resume")
	}
	if !newStorage.closed {
		t.Fatal("new storage was not closed after failed resume")
	}
}

func TestStartupPrintLinesIncludesReplayHistory(t *testing.T) {
	model := readyModel(t)
	model.App.StartupLines = []string{"line-1", "line-2"}
	model.Progress.Status = "ready"
	model.App.StartupEntries = []session.Entry{
		{Role: session.User, Content: "hello"},
		{Role: session.Agent, Content: "world"},
	}

	lines := model.startupPrintLines()
	want := []string{
		"line-1",
		"line-2",
		model.headerLine(),
		"",
		model.renderStartupStatus("ready"),
		"",
		model.renderEntry(session.Entry{Role: session.User, Content: "hello"}),
		"",
		model.renderEntry(session.Entry{Role: session.Agent, Content: "world"}),
	}

	if len(lines) != len(want) {
		t.Fatalf("startup lines length = %d, want %d", len(lines), len(want))
	}
	for i := range want {
		if lines[i] != want[i] {
			t.Fatalf("startup line %d = %q, want %q", i, lines[i], want[i])
		}
	}
}

func TestStartupPrintLinesOmitsConfigurationWarning(t *testing.T) {
	model := readyModel(t)
	model.Progress.Status = noProviderConfiguredStatus()

	lines := model.startupPrintLines()
	for _, line := range lines {
		if strings.Contains(line, "No provider configured") {
			t.Fatalf("startup lines should not include config warning: %#v", lines)
		}
	}
}

func TestProgressLineShowsConfigurationWarning(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:        &stubSession{events: make(chan session.Event)},
		provider:    "openrouter",
		providerSet: true,
		model:       "",
		modelSet:    true,
	}

	line := ansi.Strip(model.progressLine())
	if !strings.Contains(line, "No model configured") {
		t.Fatalf("progress line missing config warning: %q", line)
	}
}

func TestProgressLineIgnoresStaleConfigurationStatusWhenBackendIsConfigured(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:        &stubSession{events: make(chan session.Event)},
		provider:    "openrouter",
		providerSet: true,
		model:       "z-ai/glm-5",
		modelSet:    true,
	}
	model.Progress.Status = noModelConfiguredStatus()

	line := ansi.Strip(model.progressLine())
	if strings.Contains(line, "No model configured") {
		t.Fatalf(
			"progress line should ignore stale config warning when backend is configured: %q",
			line,
		)
	}
	if !strings.Contains(line, "Ready") {
		t.Fatalf("progress line = %q, want Ready", line)
	}
}

func TestProgressLineShowsMeaningfulRestoredStatus(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:        &stubSession{events: make(chan session.Event)},
		provider:    "openrouter",
		providerSet: true,
		model:       "z-ai/glm-5",
		modelSet:    true,
	}
	model.Progress.Status = "Running tests"

	line := ansi.Strip(model.progressLine())
	if !strings.Contains(line, "Running tests") {
		t.Fatalf("progress line missing restored status: %q", line)
	}
}

func TestProgressLineHidesBootstrapConnectedStatus(t *testing.T) {
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		sess:        &stubSession{events: make(chan session.Event)},
		provider:    "openrouter",
		providerSet: true,
		model:       "z-ai/glm-5",
		modelSet:    true,
	}
	model.Progress.Status = "Connected via Canto"

	line := ansi.Strip(model.progressLine())
	if strings.Contains(line, "Connected via Canto") {
		t.Fatalf("progress line should suppress bootstrap connection notice: %q", line)
	}
	if !strings.Contains(line, "Ready") {
		t.Fatalf("progress line = %q, want Ready", line)
	}
}
