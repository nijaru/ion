package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// TestDeterministicTUIAcceptance exercises the real Controller behind the TUI.
// It is intentionally provider-independent: the fake stream drives the same
// submit/event/tool/persist path that live providers use, while the program
// output proves terminal commits are not lost.
func TestDeterministicTUIAcceptance(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	path := t.TempDir() + "/acceptance.db"
	provider := newAcceptanceProvider(acceptanceComplete)
	store, sess, harness := newAcceptanceHarness(t, path, provider, true)
	program, output, result := startAcceptanceProgram(t, store, sess, harness)

	program.Send(tea.KeyPressMsg{Text: "run the deterministic tool"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitAcceptanceSignal(t, provider.toolStarted, "tool execution start")
	// ToolExecStart is emitted before Execute; wait for the rendered busy state
	// before testing the configured steer route so this remains deterministic on
	// a loaded test runner.
	waitForAcceptanceOutput(t, output, "Working...", "busy tool TUI state")
	program.Send(tea.KeyPressMsg{Text: "steer-now"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	// tea.Program.Send is asynchronous; let the runtime-owned steer command
	// reach the harness before releasing the blocked tool.
	time.Sleep(100 * time.Millisecond)
	close(provider.release)

	waitForAcceptanceOutput(t, output, "tool-output", "persisted tool output")
	waitForAcceptanceOutput(t, output, "final-output", "final assistant output")
	if !provider.requestContains("steer-now") {
		t.Fatal("busy input was not delivered to the next provider request")
	}

	// Exercise the actual tree picker: current leaf -> parent -> no-summary
	// navigation -> replay. The store-backed projection is what makes this
	// work with the production SQLite store, not only with test fakes.
	waitForAcceptanceIdle(t, harness)
	program.Send(tea.KeyPressMsg{Code: tea.KeyEscape})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEscape})
	waitForAcceptanceOutput(t, output, "routing_ [current]", "loaded session tree")
	for range 5 {
		program.Send(tea.KeyPressMsg{Code: tea.KeyDown})
	}
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitForAcceptanceOutput(t, output, "Summarize branch?", "branch summary prompt")
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitForAcceptanceOutput(t, output, "--- moved to branch ---", "branch replay")

	program.Quit()
	model := waitAcceptanceProgram(t, result)
	if model.Picker.Tree != nil || model.Picker.BranchSummary != nil {
		t.Fatalf("branch overlays remained open after replay: tree=%v summary=%v", model.Picker.Tree, model.Picker.BranchSummary)
	}
	if !model.App.PrintedTranscript {
		t.Fatal("terminal output was not marked as printed after branch replay")
	}
	closeAcceptanceHarness(t, harness, store)

	// Close/reopen the real SQLite store and run through a fresh Controller. The
	// fake provider records the reconstructed context, proving replay/resume
	// before the new prompt is sent.
	provider2 := newAcceptanceProvider(acceptanceResume)
	store2, err := session.NewSQLiteStore(path, "acceptance")
	if err != nil {
		t.Fatalf("reopen acceptance store: %v", err)
	}
	sess2 := session.NewSession(store2, 128)
	harness2 := agent.NewController(agent.ControllerConfig{
		Session:  sess2,
		Store:    store2,
		Durable:  store2,
		Model:    acceptanceModel(),
		StreamFn: provider2.stream,
	})
	program2, output2, result2 := startAcceptanceProgram(t, store2, sess2, harness2)
	program2.Send(tea.KeyPressMsg{Text: "resume from the persisted branch"})
	program2.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitForAcceptanceOutput(t, output2, "resumed-output", "resumed assistant output")
	if !provider2.requestContains("final-output") || !provider2.requestContains("tool-output") {
		t.Fatalf("resumed provider request did not contain the persisted branch context: %s", provider2.requestSummary())
	}
	program2.Quit()
	_ = waitAcceptanceProgram(t, result2)
	closeAcceptanceHarness(t, harness2, store2)
}

func TestDeterministicTUIAcceptanceCancelAndError(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	t.Run("cancel", func(t *testing.T) {
		path := t.TempDir() + "/cancel.db"
		provider := newAcceptanceProvider(acceptanceCancel)
		store, sess, harness := newAcceptanceHarness(t, path, provider, false)
		program, output, result := startAcceptanceProgram(t, store, sess, harness)

		program.Send(tea.KeyPressMsg{Text: "cancel this turn"})
		program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
		waitAcceptanceSignal(t, provider.streamStarted, "provider stream start")
		// The provider signal proves the turn crossed into the blocked stream.
		// Bubble Tea's incremental renderer may emit only a cursor delta for a
		// transient status change, so the raw output is not a stable place to
		// assert that the complete "Streaming..." string was written.
		waitForAcceptanceOutputAny(t, output, "cancelable TUI state", "Submitting...", "Streaming...")
		program.Send(tea.KeyPressMsg{Code: tea.KeyEscape})
		waitForAcceptanceOutput(t, output, "Canceled by user", "cancel settlement")
		waitForAcceptanceIdle(t, harness)
		time.Sleep(25 * time.Millisecond)
		program.Quit()
		model := waitAcceptanceProgram(t, result)
		if model.InFlight.Thinking {
			t.Fatal("TUI remained busy after cancellation")
		}
		closeAcceptanceHarness(t, harness, store)
	})

	t.Run("provider error", func(t *testing.T) {
		path := t.TempDir() + "/error.db"
		provider := newAcceptanceProvider(acceptanceError)
		store, sess, harness := newAcceptanceHarness(t, path, provider, false)
		program, output, result := startAcceptanceProgram(t, store, sess, harness)

		program.Send(tea.KeyPressMsg{Text: "surface the provider error"})
		program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
		waitForAcceptanceOutput(t, output, "deterministic provider failure", "provider error settlement")
		program.Quit()
		model := waitAcceptanceProgram(t, result)
		if model.InFlight.Thinking {
			t.Fatal("TUI remained busy after provider error")
		}
		closeAcceptanceHarness(t, harness, store)
	})
}

func TestDeterministicTUIAcceptanceApproval(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	path := t.TempDir() + "/approval.db"
	provider := newAcceptanceProvider(acceptanceApproval)
	store, err := session.NewSQLiteStore(path, "approval")
	if err != nil {
		t.Fatalf("open approval store: %v", err)
	}
	sess := session.NewSession(store, 128)
	harness := agent.NewController(agent.ControllerConfig{
		Session:             sess,
		Store:               store,
		Durable:             store,
		ActionJournal:       store,
		Workdir:             t.TempDir(),
		ApprovalMode:        agent.ApprovalConfirm,
		ApprovalInteractive: true,
		Model:               acceptanceModel(),
		Tools:               []agent.Tool{acceptanceApprovalTool(provider)},
		StreamFn:            provider.stream,
	})
	program, output, result := startAcceptanceProgram(t, store, sess, harness)

	program.Send(tea.KeyPressMsg{Text: "write the protected file"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitForAcceptanceOutput(t, output, "Tool approval required", "approval prompt")
	waitForAcceptanceOutput(t, output, "main.go", "approval resource")
	select {
	case <-provider.toolStarted:
		t.Fatal("approval-gated tool executed before user approval")
	default:
	}

	program.Send(tea.KeyPressMsg{Text: "y"})
	select {
	case <-provider.toolStarted:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for approved tool execution\noutput:\n%s", output.String())
	}
	waitForAcceptanceOutput(t, output, "approved-output", "approved assistant response")
	waitForAcceptanceIdle(t, harness)

	program.Quit()
	model := waitAcceptanceProgram(t, result)
	if model.Picker.Approval != nil {
		t.Fatal("approval prompt remained after approval resolution")
	}
	entries, err := sess.Entries(t.Context())
	if err != nil {
		t.Fatalf("load approval entries: %v", err)
	}
	foundToolResult := false
	for _, entry := range entries {
		messageEntry, ok := entry.(*session.MessageEntry)
		if !ok {
			continue
		}
		result, ok := messageEntry.Message.(*session.ToolResultMessage)
		if ok && result.ToolName == "write" && !result.IsError {
			foundToolResult = true
		}
	}
	if !foundToolResult {
		t.Fatalf("approved tool result was not persisted: entries=%d", len(entries))
	}
	closeAcceptanceHarness(t, harness, store)
}

func TestDeterministicTUIAcceptanceJobs(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	path := t.TempDir() + "/jobs.db"
	provider := newAcceptanceProvider(acceptanceError)
	store, sess, harness := newAcceptanceHarness(t, path, provider, false)
	jobs := &acceptanceJobs{items: []app.JobInfo{{
		ID:      "job-1",
		Command: "go test ./...",
		Status:  "running",
		Output:  "tests are running",
	}}}
	program, output, result := startAcceptanceProgramWithJobs(t, store, sess, harness, jobs)

	program.Send(tea.KeyPressMsg{Text: "/jobs"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitForAcceptanceOutput(t, output, "job-1  running", "job list")

	program.Send(tea.KeyPressMsg{Text: "/jobs stop job-1"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitForAcceptanceOutput(t, output, "Stopped background job job-1.", "job stop")
	if jobs.stopped != "job-1" {
		t.Fatalf("stopped job = %q, want job-1", jobs.stopped)
	}

	program.Quit()
	_ = waitAcceptanceProgram(t, result)
	closeAcceptanceHarness(t, harness, store)
}

func TestDeterministicTUIAcceptanceNarrowTerminal(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	path := t.TempDir() + "/narrow.db"
	provider := newAcceptanceProvider(acceptanceError)
	store, sess, harness := newAcceptanceHarness(t, path, provider, false)
	program, output, result := startAcceptanceProgram(t, store, sess, harness)

	const width = 32
	program.Send(tea.WindowSizeMsg{Width: width, Height: 24})
	program.Send(tea.KeyPressMsg{Text: "write a sentence that must wrap in a narrow terminal"})
	waitForAcceptanceOutput(t, output, "write a sentence", "narrow composer prefix")
	waitForAcceptanceOutput(t, output, "narrow terminal", "narrow composer suffix")

	program.Quit()
	model := waitAcceptanceProgram(t, result)
	if model.App.Width != width || model.App.Height != 24 {
		t.Fatalf("terminal size = %dx%d, want %dx%d", model.App.Width, model.App.Height, width, 24)
	}
	if got := model.Input.Composer.Value(); got != "write a sentence that must wrap in a narrow terminal" {
		t.Fatalf("composer value = %q, want the complete narrow-terminal prompt", got)
	}
	if got := model.Input.Composer.Height(); got < 2 {
		t.Fatalf("composer height = %d, want soft-wrapped input to occupy multiple rows", got)
	}

	view := ansi.Strip(model.View().Content)
	for lineNumber, line := range strings.Split(view, "\n") {
		if got := ansi.StringWidth(line); got > width {
			t.Fatalf("rendered line %d width = %d, want <= %d:\n%s", lineNumber+1, got, width, view)
		}
	}
	closeAcceptanceHarness(t, harness, store)
}

func TestDeterministicTUIAcceptanceRuntimeSwitches(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	path := t.TempDir() + "/switches.db"
	store, err := session.NewSQLiteStore(path, "stable-runtime-id")
	if err != nil {
		t.Fatalf("open switching store: %v", err)
	}
	initialProvider := newAcceptanceTextProvider("model-a-output")
	resumedProvider := newAcceptanceTextProvider("model-a-resumed-output")
	modelBProvider := newAcceptanceTextProvider("model-b-output")
	postResumeModelBProvider := newAcceptanceTextProvider("post-resume-model-b-output")
	modelBSwitches := 0
	initialSession := session.NewSession(store, 128)
	initialConfig := &config.Config{Provider: "openai", Model: "model-a", BusyInput: "steer"}
	initialRunner := newAcceptanceRuntime(store, initialSession, initialConfig, initialProvider)
	switches := make(chan acceptanceRuntimeSwitch, 4)
	switcher := func(ctx context.Context, cfg *config.Config, leafID string) (app.RuntimeInfo, agent.Runtime, app.RuntimeStorage, error) {
		if leafID != "" {
			if err := store.ResumeSession(ctx, leafID); err != nil {
				return nil, nil, nil, err
			}
		}
		provider := resumedProvider
		if cfg.Model == "model-b" {
			provider = modelBProvider
			if modelBSwitches > 0 {
				provider = postResumeModelBProvider
			}
			modelBSwitches++
		}
		switchedSession := session.NewSession(store, 128)
		runner := newAcceptanceRuntime(store, switchedSession, cfg, provider)
		switches <- acceptanceRuntimeSwitch{model: cfg.Model, leafID: leafID, runner: runner}
		return acceptanceBackend(cfg), runner, switchedSession, nil
	}

	model := app.New(
		acceptanceBackend(initialConfig),
		initialSession,
		store,
		t.TempDir(),
		"main",
		"test",
		switcher,
	).WithRunner(initialRunner).WithConfig(initialConfig)
	program, output, result := startAcceptanceAppProgram(t, model)

	program.Send(tea.KeyPressMsg{Text: "initial runtime turn"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitAcceptanceSignal(t, initialProvider.started, "initial runtime provider")
	waitForAcceptanceOutput(t, output, "model-a-output", "initial runtime output")
	leafID := store.GetLeafID()
	if leafID == "" {
		t.Fatal("initial turn did not materialize a tree leaf")
	}
	waitForAcceptanceSessionInfo(t, store, leafID)

	program.Send(tea.KeyPressMsg{Text: "/model model-b"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitForAcceptanceOutput(t, output, "Model set to model-b", "model switch notice")
	switchResult := waitAcceptanceRuntimeSwitch(t, switches, "model-b")
	if switchResult.leafID != leafID {
		t.Fatalf("model switch leaf = %q, want current leaf %q", switchResult.leafID, leafID)
	}
	program.Send(tea.KeyPressMsg{Text: "after model switch"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitAcceptanceSignal(t, modelBProvider.started, "model-b provider")
	waitForAcceptanceOutput(t, output, "model-b-output", "model-b runtime output")

	program.Send(tea.KeyPressMsg{Text: "/resume " + leafID})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitForAcceptanceOutput(t, output, "--- resumed ---", "session resume replay boundary")
	resumeResult := waitAcceptanceRuntimeSwitch(t, switches, "model-a")
	if resumeResult.leafID != leafID {
		t.Fatalf("resume leaf = %q, want selected leaf %q", resumeResult.leafID, leafID)
	}
	program.Send(tea.KeyPressMsg{Text: "after session resume"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitAcceptanceSignal(t, resumedProvider.started, "resumed runtime provider")
	waitForAcceptanceOutput(t, output, "model-a-resumed-output", "resumed runtime output")
	postResumeLeafID := store.GetLeafID()
	if postResumeLeafID == "" {
		t.Fatal("resumed turn did not materialize a tree leaf")
	}
	waitForAcceptanceSessionInfo(t, store, postResumeLeafID)
	modelSwitchNoticeCount := strings.Count(output.String(), "Model set to model-b")
	program.Send(tea.KeyPressMsg{Text: "/model model-b"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitForAcceptanceOutputAfter(t, output, "Model set to model-b", modelSwitchNoticeCount, "post-resume model switch notice")
	postResumeSwitch := waitAcceptanceRuntimeSwitch(t, switches, "model-b")
	if postResumeSwitch.leafID != postResumeLeafID {
		t.Fatalf("post-resume model switch leaf = %q, want %q", postResumeSwitch.leafID, postResumeLeafID)
	}
	program.Send(tea.KeyPressMsg{Text: "after post-resume model switch"})
	program.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
	waitAcceptanceSignal(t, postResumeModelBProvider.started, "post-resume model-b provider")
	waitForAcceptanceOutput(t, output, "post-resume-model-b-output", "post-resume model-b runtime output")

	program.Quit()
	finalModel := waitAcceptanceProgram(t, result)
	if got := finalModel.Model.Info.Model(); got != "model-b" {
		t.Fatalf("final runtime model = %q, want model-b after post-resume switch", got)
	}
	finalRunner, ok := finalModel.Model.Runner.(*agent.Controller)
	if !ok {
		t.Fatalf("final runtime runner = %T, want *agent.Controller", finalModel.Model.Runner)
	}
	closeAcceptanceHarness(t, finalRunner, store)
}

type acceptanceResult struct {
	model tea.Model
	err   error
}

type acceptanceBuffer struct {
	mu sync.Mutex
	bytes.Buffer
}

func (b *acceptanceBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.Buffer.Write(p)
}

// Bubble Tea prefers io.StringWriter when the output provides it. Because the
// embedded bytes.Buffer promotes WriteString and WriteByte, those methods
// would otherwise bypass Write's mutex and race with String during polling.
func (b *acceptanceBuffer) WriteString(value string) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.Buffer.WriteString(value)
}

func (b *acceptanceBuffer) WriteByte(value byte) error {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.Buffer.WriteByte(value)
}

func (b *acceptanceBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.Buffer.String()
}

func startAcceptanceProgram(
	t *testing.T,
	catalog agent.SessionCatalog,
	sess session.Session,
	runner agent.Runtime,
) (*tea.Program, *acceptanceBuffer, <-chan acceptanceResult) {
	return startAcceptanceProgramWithJobs(t, catalog, sess, runner, nil)
}

func startAcceptanceProgramWithJobs(
	t *testing.T,
	catalog agent.SessionCatalog,
	sess session.Session,
	runner agent.Runtime,
	jobs app.JobController,
) (*tea.Program, *acceptanceBuffer, <-chan acceptanceResult) {
	t.Helper()
	backend := newSmokeBackend("complete")
	cfg := &config.Config{Provider: "fake", Model: "fake-model", BusyInput: "steer"}
	model := app.New(backend, sess, catalog, "/tmp/ion-acceptance", "main", "test", nil).
		WithRunner(runner).
		WithJobs(jobs).
		WithConfig(cfg)
	return startAcceptanceAppProgram(t, model)
}

func startAcceptanceAppProgram(
	t *testing.T,
	model app.Model,
) (*tea.Program, *acceptanceBuffer, <-chan acceptanceResult) {
	t.Helper()
	output := &acceptanceBuffer{}
	program := tea.NewProgram(
		&model,
		tea.WithInput(nil),
		tea.WithOutput(output),
		tea.WithoutSignals(),
		tea.WithWindowSize(120, 40),
	)
	result := make(chan acceptanceResult, 1)
	go func() {
		returned, err := program.Run()
		result <- acceptanceResult{model: returned, err: err}
	}()
	// Ensure the program has entered its event loop before the first Send.
	time.Sleep(20 * time.Millisecond)
	program.Send(tea.WindowSizeMsg{Width: 120, Height: 40})
	return program, output, result
}

type acceptanceRuntimeSwitch struct {
	model  string
	leafID string
	runner *agent.Controller
}

func waitAcceptanceRuntimeSwitch(
	t *testing.T,
	switches <-chan acceptanceRuntimeSwitch,
	wantModel string,
) acceptanceRuntimeSwitch {
	t.Helper()
	select {
	case result := <-switches:
		if result.model != wantModel {
			t.Fatalf("runtime switch model = %q, want %q", result.model, wantModel)
		}
		return result
	case <-time.After(10 * time.Second):
		t.Fatalf("timed out waiting for runtime switch to %s", wantModel)
		return acceptanceRuntimeSwitch{}
	}
}

func waitForAcceptanceSessionInfo(t *testing.T, catalog agent.SessionCatalog, leafID string) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		info, err := catalog.GetSessionInfo(context.Background(), leafID)
		if err == nil && info.ID() == leafID {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for catalog entry %q", leafID)
}

type acceptanceJobs struct {
	items   []app.JobInfo
	stopped string
}

func (j *acceptanceJobs) ListJobs() []app.JobInfo {
	return append([]app.JobInfo(nil), j.items...)
}

func (j *acceptanceJobs) StopJob(id string) error {
	j.stopped = id
	for i := range j.items {
		if j.items[i].ID == id {
			j.items[i].Status = "canceled"
		}
	}
	return nil
}

func waitAcceptanceSignal(t *testing.T, signal <-chan struct{}, label string) {
	t.Helper()
	select {
	case <-signal:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for %s", label)
	}
}

func waitForAcceptanceIdle(t *testing.T, harness *agent.Controller) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if harness.IsIdle() {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatal("runtime remained busy after cancellation")
}

func waitForAcceptanceOutput(t *testing.T, output *acceptanceBuffer, needle, label string) string {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		content := output.String()
		if strings.Contains(content, needle) {
			return content
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s %q\noutput:\n%s", label, needle, output.String())
	return ""
}

func waitForAcceptanceOutputAfter(
	t *testing.T,
	output *acceptanceBuffer,
	needle string,
	previousCount int,
	label string,
) string {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		content := output.String()
		if strings.Count(content, needle) > previousCount {
			return content
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s %q after %d occurrences\noutput:\n%s", label, needle, previousCount, output.String())
	return ""
}

func waitForAcceptanceOutputAny(t *testing.T, output *acceptanceBuffer, label string, needles ...string) string {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		content := output.String()
		for _, needle := range needles {
			if strings.Contains(content, needle) {
				return content
			}
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for %s %q\noutput:\n%s", label, needles, output.String())
	return ""
}

func waitAcceptanceProgram(t *testing.T, result <-chan acceptanceResult) *app.Model {
	t.Helper()
	select {
	case returned := <-result:
		if returned.err != nil {
			t.Fatalf("deterministic TUI program: %v", returned.err)
		}
		model, ok := returned.model.(*app.Model)
		if !ok || model == nil {
			t.Fatalf("deterministic TUI returned %T, want *app.Model", returned.model)
		}
		return model
	case <-time.After(10 * time.Second):
		t.Fatal("timed out waiting for deterministic TUI program")
		return nil
	}
}

func newAcceptanceHarness(
	t *testing.T,
	path string,
	provider *acceptanceProvider,
	withTool bool,
) (*session.SQLiteStore, session.Session, *agent.Controller) {
	t.Helper()
	store, err := session.NewSQLiteStore(path, "acceptance")
	if err != nil {
		t.Fatalf("open acceptance store: %v", err)
	}
	sess := session.NewSession(store, 128)
	var tools []agent.Tool
	if withTool {
		tools = []agent.Tool{acceptanceTool(provider)}
	}
	harness := agent.NewController(agent.ControllerConfig{
		Session:  sess,
		Store:    store,
		Durable:  store,
		Model:    acceptanceModel(),
		Tools:    tools,
		StreamFn: provider.stream,
	})
	return store, sess, harness
}

func closeAcceptanceHarness(t *testing.T, harness *agent.Controller, store *session.SQLiteStore) {
	t.Helper()
	if err := harness.Close(); err != nil {
		t.Fatalf("close acceptance harness: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close acceptance store: %v", err)
	}
}

func acceptanceModel() llm.Model {
	return acceptanceModelNamed("fake", "fake-model")
}

func acceptanceModelNamed(provider, model string) llm.Model {
	return llm.Model{
		ID:            model,
		Provider:      provider,
		API:           "fake",
		ContextWindow: 128000,
	}
}

func acceptanceBackend(cfg *config.Config) *smokeBackend {
	backend := newSmokeBackend("complete")
	if cfg != nil {
		copy := *cfg
		backend.cfg = &copy
	}
	return backend
}

func newAcceptanceRuntime(
	store *session.SQLiteStore,
	sess session.Session,
	cfg *config.Config,
	provider *acceptanceTextProvider,
) *agent.Controller {
	return agent.NewController(agent.ControllerConfig{
		Session:  sess,
		Store:    store,
		Durable:  store,
		Model:    acceptanceModelNamed(cfg.Provider, cfg.Model),
		StreamFn: provider.stream,
	})
}

type acceptanceTextProvider struct {
	response string
	started  chan struct{}
	once     sync.Once
}

func newAcceptanceTextProvider(response string) *acceptanceTextProvider {
	return &acceptanceTextProvider{
		response: response,
		started:  make(chan struct{}),
	}
}

func (p *acceptanceTextProvider) stream(context.Context, *llm.Request) (llm.Stream, error) {
	p.once.Do(func() { close(p.started) })
	return &acceptanceStream{chunks: []*llm.Chunk{{Content: p.response, StopReason: "stop"}}}, nil
}

type acceptanceMode string

const (
	acceptanceComplete acceptanceMode = "complete"
	acceptanceResume   acceptanceMode = "resume"
	acceptanceCancel   acceptanceMode = "cancel"
	acceptanceError    acceptanceMode = "error"
	acceptanceApproval acceptanceMode = "approval"
)

type acceptanceProvider struct {
	mode acceptanceMode

	mu       sync.Mutex
	calls    int
	requests []llm.Request

	streamStarted chan struct{}
	streamOnce    sync.Once
	toolStarted   chan struct{}
	toolOnce      sync.Once
	release       chan struct{}
}

func newAcceptanceProvider(mode acceptanceMode) *acceptanceProvider {
	return &acceptanceProvider{
		mode:          mode,
		streamStarted: make(chan struct{}),
		toolStarted:   make(chan struct{}),
		release:       make(chan struct{}),
	}
}

func (p *acceptanceProvider) stream(ctx context.Context, req *llm.Request) (llm.Stream, error) {
	p.mu.Lock()
	p.calls++
	call := p.calls
	snapshot := *req
	snapshot.Messages = append([]llm.Message(nil), req.Messages...)
	p.requests = append(p.requests, snapshot)
	p.mu.Unlock()

	switch p.mode {
	case acceptanceComplete:
		if call == 1 {
			var function struct {
				Name      string `json:"name"`
				Arguments string `json:"arguments"`
			}
			function.Name = "echo"
			function.Arguments = `{"text":"tool-input"}`
			return &acceptanceStream{chunks: []*llm.Chunk{{
				Calls:      []llm.Call{{ID: "call-1", Type: "function", Function: function}},
				StopReason: "toolUse",
			}}}, nil
		}
		return &acceptanceStream{chunks: []*llm.Chunk{{Content: "final-output", StopReason: "stop"}}}, nil
	case acceptanceResume:
		return &acceptanceStream{chunks: []*llm.Chunk{{Content: "resumed-output", StopReason: "stop"}}}, nil
	case acceptanceCancel:
		p.streamOnce.Do(func() { close(p.streamStarted) })
		<-ctx.Done()
		return nil, ctx.Err()
	case acceptanceError:
		return nil, errors.New("deterministic provider failure")
	case acceptanceApproval:
		if call == 1 {
			return &acceptanceStream{chunks: []*llm.Chunk{{
				Calls: []llm.Call{{
					ID:   "approval-call-1",
					Type: "function",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "write", Arguments: `{"path":"main.go"}`},
				}},
				StopReason: "toolUse",
			}}}, nil
		}
		return &acceptanceStream{chunks: []*llm.Chunk{{Content: "approved-output", StopReason: "stop"}}}, nil
	default:
		return nil, fmt.Errorf("unknown acceptance mode %q", p.mode)
	}
}

func (p *acceptanceProvider) requestContains(needle string) bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	for _, request := range p.requests {
		for _, message := range request.Messages {
			if strings.Contains(message.TextContent(), needle) {
				return true
			}
		}
	}
	return false
}

func (p *acceptanceProvider) requestSummary() string {
	p.mu.Lock()
	defer p.mu.Unlock()
	var summary []string
	for _, request := range p.requests {
		var messages []string
		for _, message := range request.Messages {
			messages = append(messages, string(message.Role)+":"+message.TextContent())
		}
		summary = append(summary, strings.Join(messages, " | "))
	}
	return strings.Join(summary, " || ")
}

func acceptanceTool(provider *acceptanceProvider) agent.Tool {
	return agent.Tool{
		Name:        "echo",
		Description: "Return deterministic tool output",
		Parameters:  `{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}`,
		Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
			provider.toolOnce.Do(func() { close(provider.toolStarted) })
			select {
			case <-provider.release:
				return session.ToolResultMessage{
					ToolCallID: id,
					ToolName:   "echo",
					Content:    []session.Content{session.TextContent{Text: "tool-output"}},
				}, nil
			case <-ctx.Done():
				return session.ToolResultMessage{}, ctx.Err()
			case <-signal:
				return session.ToolResultMessage{}, context.Canceled
			}
		},
	}
}

func acceptanceApprovalTool(provider *acceptanceProvider) agent.Tool {
	return agent.Tool{
		Name:           "write",
		Description:    "Write deterministic protected content",
		RequiresAction: true,
		Parameters:     `{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}`,
		ApprovalRequirement: func(json.RawMessage) (agent.ApprovalRequirement, bool, error) {
			return agent.ApprovalRequirement{
				Category:  "write",
				Operation: "write",
				Resource:  "main.go",
			}, true, nil
		},
		Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
			provider.toolOnce.Do(func() { close(provider.toolStarted) })
			return session.ToolResultMessage{
				ToolName: "write",
				Content:  []session.Content{session.TextContent{Text: "protected-write-complete"}},
			}, nil
		},
	}
}

type acceptanceStream struct {
	chunks []*llm.Chunk
	index  int
}

func (s *acceptanceStream) Next() (*llm.Chunk, bool) {
	if s.index >= len(s.chunks) {
		return nil, false
	}
	chunk := s.chunks[s.index]
	s.index++
	return chunk, true
}

func (s *acceptanceStream) Err() error   { return nil }
func (s *acceptanceStream) Close() error { return nil }
