package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/agent"
	ionexport "github.com/nijaru/ion/internal/export"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// printSession implements both session.Session and agent.Runtime for testing print mode.
type printSession struct {
	events    chan agent.EventEnvelope
	cancelled int
	closed    int
	submitErr error
}

func printEnvelope(event session.Event) agent.EventEnvelope {
	return agent.EventEnvelope{Event: event}
}

type abortReleasingPrintSession struct {
	*printSession
	unblock           chan struct{}
	abortCalled       chan struct{}
	globalAbortCalled chan struct{}
	once              sync.Once
	globalAbortOnce   sync.Once
}

type gatedPrintSession struct {
	*printSession
	release chan struct{}
}

type lateAcceptancePrintSession struct {
	*printSession
	promptStarted     chan struct{}
	acceptanceRelease chan struct{}
	settledPublished  chan struct{}
	releaseOnce       sync.Once
}

func (s *lateAcceptancePrintSession) Prompt(
	ctx context.Context,
	_ string,
	_ ...session.ImageContent,
) (session.Message, error) {
	close(s.promptStarted)
	if sink := agent.TurnTokenSinkFromContext(ctx); sink != nil {
		sink(1)
	}
	<-s.acceptanceRelease
	if sink := agent.TurnAcceptanceSinkFromContext(ctx); sink != nil {
		sink()
	}
	go func() {
		time.Sleep(20 * time.Millisecond)
		close(s.settledPublished)
		s.events <- printEnvelope(session.Settled{})
	}()
	return nil, context.Canceled
}

func (s *lateAcceptancePrintSession) Abort() ([]session.Message, []session.Message, error) {
	s.releaseOnce.Do(func() { close(s.acceptanceRelease) })
	return nil, nil, nil
}

func (s *lateAcceptancePrintSession) AbortTurn(uint64) ([]session.Message, []session.Message, error) {
	return s.Abort()
}

func (s *gatedPrintSession) Prompt(ctx context.Context, _ string, _ ...session.ImageContent) (session.Message, error) {
	if sink := agent.TurnTokenSinkFromContext(ctx); sink != nil {
		sink(1)
	}
	<-s.release
	return &session.AssistantMessage{Content: []session.Content{session.TextContent{Text: "done"}}}, nil
}

type signalWriter struct {
	mu         sync.Mutex
	buffer     bytes.Buffer
	firstWrite chan struct{}
	once       sync.Once
}

func (w *signalWriter) Write(p []byte) (int, error) {
	w.once.Do(func() { close(w.firstWrite) })
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.buffer.Write(p)
}

func (w *signalWriter) WriteString(s string) (int, error) {
	w.once.Do(func() { close(w.firstWrite) })
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.buffer.WriteString(s)
}

func (w *signalWriter) String() string {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.buffer.String()
}

func (s *abortReleasingPrintSession) Prompt(
	ctx context.Context,
	_ string,
	_ ...session.ImageContent,
) (session.Message, error) {
	if sink := agent.TurnTokenSinkFromContext(ctx); sink != nil {
		sink(1)
	}
	<-s.unblock
	return nil, errors.New("prompt aborted")
}

func (s *abortReleasingPrintSession) Abort() ([]session.Message, []session.Message, error) {
	s.globalAbortOnce.Do(func() {
		if s.globalAbortCalled != nil {
			close(s.globalAbortCalled)
		}
	})
	return nil, nil, errors.New("global abort must not be used")
}

func (s *abortReleasingPrintSession) AbortTurn(uint64) ([]session.Message, []session.Message, error) {
	s.once.Do(func() {
		s.cancelled++
		close(s.unblock)
		if s.abortCalled != nil {
			close(s.abortCalled)
		}
	})
	return nil, nil, nil
}

func (s *printSession) ID() string                                  { return "print-test" }
func (s *printSession) SessionID() string                           { return s.ID() }
func (s *printSession) Meta() session.Metadata                      { return session.Metadata{} }
func (s *printSession) SessionName(context.Context) (string, error) { return "", nil }
func (s *printSession) BuildContext(context.Context) (session.ContextSnapshot, error) {
	return session.ContextSnapshot{}, nil
}
func (s *printSession) Branch(context.Context) ([]session.Entry, error)        { return nil, nil }
func (s *printSession) SessionBranch(context.Context) ([]session.Entry, error) { return nil, nil }
func (s *printSession) SessionTree(context.Context) (agent.SessionTreeSnapshot, error) {
	return agent.SessionTreeSnapshot{}, nil
}

func (s *printSession) BranchAt(context.Context, string) ([]session.Entry, error) { return nil, nil }

func (s *printSession) AppendMessage(context.Context, session.Message) (string, error) {
	return "", nil
}

func (s *printSession) AppendModelChange(context.Context, string, string) (string, error) {
	return "", nil
}

func (s *printSession) AppendThinkingLevelChange(context.Context, session.ThinkingLevel) (string, error) {
	return "", nil
}

func (s *printSession) AppendActiveToolsChange(context.Context, []string) (string, error) {
	return "", nil
}

func (s *printSession) AppendCompaction(context.Context, session.CompactionData) (string, error) {
	return "", nil
}

func (s *printSession) AppendBranchSummary(context.Context, session.BranchSummaryData) (string, error) {
	return "", nil
}

func (s *printSession) AppendLabel(context.Context, string, string) (string, error) {
	return "", nil
}

func (s *printSession) AppendSessionInfo(context.Context, string) (string, error) {
	return "", nil
}

func (s *printSession) AppendCustom(context.Context, *session.CustomEntry) (string, error) {
	return "", nil
}

func (s *printSession) AppendLeaf(context.Context, string) (string, error) {
	return "", nil
}

func (s *printSession) AppendCustomMessage(context.Context, *session.CustomMessageEntry) (string, error) {
	return "", nil
}

func (s *printSession) GetLabel(context.Context, string) (string, error) {
	return "", nil
}

func (s *printSession) Append(context.Context, session.Entry) (string, error) {
	return "", nil
}

func (s *printSession) SubmitTurn(ctx context.Context, turn string) error {
	if s.submitErr != nil {
		return s.submitErr
	}
	return nil
}

func (s *printSession) CancelTurn(context.Context) error {
	s.cancelled++
	return nil
}

func (s *printSession) Subscribe(context.Context, agent.EventCursor) (*agent.EventSubscription, error) {
	return &agent.EventSubscription{Events: s.events}, nil
}

func (s *printSession) GetEntry(context.Context, string) (session.Entry, error) {
	return nil, nil
}
func (s *printSession) GetLeafID() string      { return "" }
func (s *printSession) SetLeafID(string) error { return nil }
func (s *printSession) MoveTo(context.Context, string, *session.BranchSummaryData) (string, error) {
	return "", nil
}

func (s *printSession) NavigateTree(context.Context, string, agent.NavigateOptions) (agent.NavigateResult, error) {
	return agent.NavigateResult{}, nil
}
func (s *printSession) Entries(context.Context) ([]session.Entry, error) { return nil, nil }
func (s *printSession) Usage(context.Context) (session.Usage, error) {
	return session.Usage{}, nil
}

func (s *printSession) Close() error {
	s.closed++
	return nil
}

// --- agent.Runtime implementation ---

func (s *printSession) Prompt(ctx context.Context, _ string, _ ...session.ImageContent) (session.Message, error) {
	if sink := agent.TurnTokenSinkFromContext(ctx); sink != nil {
		sink(1)
	}
	if s.submitErr != nil {
		return nil, s.submitErr
	}
	return nil, nil
}
func (s *printSession) Steer(_ string, _ ...session.ImageContent) error    { return nil }
func (s *printSession) FollowUp(_ string, _ ...session.ImageContent) error { return nil }
func (s *printSession) NextTurn(_ string, _ ...session.ImageContent) error { return nil }
func (s *printSession) ActiveTurnToken() uint64                            { return 1 }
func (s *printSession) AbortTurn(uint64) ([]session.Message, []session.Message, error) {
	return s.Abort()
}

func (s *printSession) Abort() ([]session.Message, []session.Message, error) {
	s.cancelled++
	return nil, nil, nil
}
func (s *printSession) SetModel(_ llm.Model) error { return nil }
func (s *printSession) SetThinking(context.Context, session.ThinkingLevel) (string, error) {
	return "", nil
}
func (s *printSession) SetTools(_ []agent.Tool, _ []string) error     { return nil }
func (s *printSession) ActivateTools(context.Context, []string) error { return nil }
func (s *printSession) Session() session.Session                      { return s }
func (s *printSession) ForkSession(context.Context, string) (string, error) {
	return "", nil
}

func (s *printSession) ImportSessionBundle(context.Context, ionexport.SessionBundle) (string, error) {
	return "", nil
}
func (s *printSession) Compact(_ context.Context) error { return nil }

func TestResolvePrintFlagsSupportsShortPrintWithPositionalPrompt(t *testing.T) {
	requested, prompt, output, err := resolvePrintFlags(
		false,
		true,
		"",
		[]string{"hello"},
		"text",
		false,
	)
	if err != nil {
		t.Fatalf("resolve print flags: %v", err)
	}
	if !requested || prompt != "hello" || output != "text" {
		t.Fatalf(
			"requested=%v prompt=%q output=%q, want print hello text",
			requested,
			prompt,
			output,
		)
	}
}

func TestResolvePrintFlagsSupportsJSONShortcut(t *testing.T) {
	requested, prompt, output, err := resolvePrintFlags(false, false, "hello", nil, "text", true)
	if err != nil {
		t.Fatalf("resolve print flags: %v", err)
	}
	if !requested || prompt != "hello" || output != "json" {
		t.Fatalf(
			"requested=%v prompt=%q output=%q, want print hello json",
			requested,
			prompt,
			output,
		)
	}
}

func TestResolvePrintFlagsUsesPositionalPromptInPrintMode(t *testing.T) {
	requested, prompt, output, err := resolvePrintFlags(
		true,
		false,
		"",
		[]string{"hello", "world"},
		"",
		false,
	)
	if err != nil {
		t.Fatalf("resolve print flags: %v", err)
	}
	if !requested || prompt != "hello world" || output != "text" {
		t.Fatalf(
			"requested=%v prompt=%q output=%q, want joined positional prompt",
			requested,
			prompt,
			output,
		)
	}
}

func TestResolvePrintFlagsRejectsUnexpectedArguments(t *testing.T) {
	_, _, _, err := resolvePrintFlags(false, false, "", []string{"hello"}, "text", false)
	if err == nil || !strings.Contains(err.Error(), "unexpected arguments") {
		t.Fatalf("resolve print flags error = %v", err)
	}
}

func TestResolvePrintFlagsRejectsPromptWithExtraArguments(t *testing.T) {
	_, _, _, err := resolvePrintFlags(false, false, "hello", []string{"ignored"}, "text", false)
	if err == nil || !strings.Contains(err.Error(), "unexpected arguments after --prompt") {
		t.Fatalf("resolve print flags error = %v", err)
	}
}

func TestResolvePrintFlagsRejectsUnsupportedOutputBeforePrint(t *testing.T) {
	_, _, _, err := resolvePrintFlags(true, false, "hello", nil, "xml", false)
	if err == nil || !strings.Contains(err.Error(), `unsupported print output "xml"`) {
		t.Fatalf("resolve print flags error = %v", err)
	}
}

func TestValidatePrintTimeoutRequiresPositiveDuration(t *testing.T) {
	tests := []struct {
		name           string
		printRequested bool
		timeout        time.Duration
		wantError      string
	}{
		{name: "default", printRequested: true, timeout: 5 * time.Minute},
		{name: "positive", printRequested: true, timeout: time.Millisecond},
		{name: "interactive ignores timeout", printRequested: false, timeout: 0},
		{
			name:           "zero",
			printRequested: true,
			timeout:        0,
			wantError:      "--timeout must be greater than zero in print mode (got 0s)",
		},
		{
			name:           "negative",
			printRequested: true,
			timeout:        -time.Second,
			wantError:      "--timeout must be greater than zero in print mode (got -1s)",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := validatePrintTimeout(tt.printRequested, tt.timeout)
			if tt.wantError == "" {
				if err != nil {
					t.Fatalf("validatePrintTimeout() = %v, want nil", err)
				}
				return
			}
			if err == nil || err.Error() != tt.wantError {
				t.Fatalf("validatePrintTimeout() = %v, want %q", err, tt.wantError)
			}
		})
	}
}

func TestNormalizeFlagArgsAllowsFlagsAfterPositionalPrompt(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{
		"--print",
		"reply with ok",
		"--json",
		"--timeout",
		"30s",
	})
	want := []string{"--print", "--json", "--timeout", "30s", "--", "reply with ok"}
	if openResumePicker {
		t.Fatal("normalizeFlagArgs opened resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}

	got, openResumePicker = normalizeFlagArgs([]string{"-p", "--agent"})
	want = []string{"-p", "--", "--agent"}
	if openResumePicker {
		t.Fatal("normalizeFlagArgs opened resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestNormalizeFlagArgsKeepsPromptValuesWithFlags(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{"-p", "reply with ok", "--json"})
	want := []string{"-p", "--json", "--", "reply with ok"}
	if openResumePicker {
		t.Fatal("normalizeFlagArgs opened resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestNormalizeFlagArgsAllowsShortPrintBeforeOtherFlags(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{"-p", "--json", "reply with ok"})
	want := []string{"-p", "--json", "--", "reply with ok"}
	if openResumePicker {
		t.Fatal("normalizeFlagArgs opened resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestNormalizeFlagArgsKeepsUnknownFlagBeforePrintForParser(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{"--bad", "-p", "reply with ok"})
	want := []string{"--bad", "-p", "--", "reply with ok"}
	if openResumePicker {
		t.Fatal("normalizeFlagArgs opened resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestNormalizeFlagArgsAllowsDashPromptAfterPrint(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{"-p", "--literal-prompt"})
	want := []string{"-p", "--", "--literal-prompt"}
	if openResumePicker {
		t.Fatal("normalizeFlagArgs opened resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestNormalizeFlagArgsTreatsRemovedModeFlagsAsPromptText(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{"-p", "--mode", "read"})
	want := []string{"-p", "--", "--mode", "read"}
	if openResumePicker {
		t.Fatal("normalizeFlagArgs opened resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}

	got, openResumePicker = normalizeFlagArgs([]string{"-p", "--yolo"})
	want = []string{"-p", "--", "--yolo"}
	if openResumePicker {
		t.Fatal("normalizeFlagArgs opened resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestNormalizeFlagArgsKeepsUnknownFlagAfterPromptForParser(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{"--prompt", "hello", "--bad"})
	want := []string{"--prompt", "hello", "--bad"}
	if openResumePicker {
		t.Fatal("normalizeFlagArgs opened resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestNormalizeFlagArgsSupportsBareResumePickerWithInterspersedFlags(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{"--resume", "--print", "hello", "--json"})
	want := []string{"--print", "--json", "--", "hello"}
	if !openResumePicker {
		t.Fatal("normalizeFlagArgs did not open resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestNormalizeFlagArgsSupportsBareShortResumePickerWithInterspersedFlags(t *testing.T) {
	got, openResumePicker := normalizeFlagArgs([]string{"-r", "-p", "hello", "--json"})
	want := []string{"-p", "--json", "--", "hello"}
	if !openResumePicker {
		t.Fatal("normalizeFlagArgs did not open resume picker")
	}
	if !slices.Equal(got, want) {
		t.Fatalf("normalizeFlagArgs = %#v, want %#v", got, want)
	}
}

func TestValidatePrintSelectionRejectsBareResumeInPrintMode(t *testing.T) {
	err := validatePrintSelection(true, true)
	if err == nil ||
		!strings.Contains(err.Error(), "--resume requires a session ID in print mode") {
		t.Fatalf("validatePrintSelection error = %v", err)
	}
	if err := validatePrintSelection(false, true); err != nil {
		t.Fatalf("TUI resume picker should be valid: %v", err)
	}
	if err := validatePrintSelection(true, false); err != nil {
		t.Fatalf("explicit print session should be valid: %v", err)
	}
}

func TestValidateSessionSelectionRejectsConflicts(t *testing.T) {
	if err := validateSessionSelection(true, "", "", "", false, false, "", ""); err != nil {
		t.Fatalf("no-session alone should be valid: %v", err)
	}
	if err := validateSessionSelection(
		true,
		"session-1",
		"",
		"",
		false,
		false,
		"",
		"",
	); err == nil || !strings.Contains(err.Error(), "--no-session cannot be combined") {
		t.Fatalf("no-session/session error = %v", err)
	}
	if err := validateSessionSelection(
		true,
		"",
		"",
		"",
		false,
		false,
		"bundle.json",
		"",
	); err == nil || !strings.Contains(err.Error(), "--no-session cannot be combined") {
		t.Fatalf("no-session/export error = %v", err)
	}
	if err := validateSessionSelection(
		false,
		"session-1",
		"resume-1",
		"",
		false,
		false,
		"",
		"",
	); err == nil || !strings.Contains(err.Error(), "--session cannot be combined") {
		t.Fatalf("session/resume error = %v", err)
	}
	if err := validateSessionSelection(
		false,
		"session-1",
		"",
		"",
		true,
		false,
		"",
		"",
	); err == nil || !strings.Contains(err.Error(), "--session cannot be combined") {
		t.Fatalf("session/continue error = %v", err)
	}
	if err := validateSessionSelection(false, "", "", "", false, false, "bundle.json", ""); err == nil ||
		!strings.Contains(err.Error(), "--export-session requires") {
		t.Fatalf("export without selection error = %v", err)
	}
}

// Print mode is non-interactive; requirement-bearing tools are denied by the
// harness approval broker and persist a recoverable tool error.

func TestPrintModeWritesTextOutput(t *testing.T) {
	sess := &printSession{events: make(chan agent.EventEnvelope, 4)}
	sess.events <- printEnvelope(session.MessageUpdate{
		Delta:     session.TextDelta{Text: "hello"},
		BlockType: "text",
	})
	sess.events <- printEnvelope(session.MessageUpdate{
		Delta:     session.TextDelta{Text: " world"},
		BlockType: "text",
	})
	sess.events <- printEnvelope(session.TurnEnd{Base: session.BaseNow()})
	sess.events <- printEnvelope(session.Settled{})

	var out bytes.Buffer
	if err := runPrintModeWithWriter(context.Background(), &out, sess, "hello", "text"); err != nil {
		t.Fatalf("runPrintMode returned error: %v", err)
	}
	if got := out.String(); got != "hello world\n" {
		t.Fatalf("text output = %q, want hello world newline", got)
	}
}

func TestPrintModeStreamsTypedTextDeltaWithoutBlockType(t *testing.T) {
	sess := &printSession{events: make(chan agent.EventEnvelope, 3)}
	sess.events <- printEnvelope(session.MessageUpdate{
		Delta: session.TextDelta{Text: "typed"},
	})
	sess.events <- printEnvelope(session.TurnEnd{Base: session.BaseNow()})
	sess.events <- printEnvelope(session.Settled{})

	var out bytes.Buffer
	if err := runPrintModeWithWriter(context.Background(), &out, sess, "hello", "text"); err != nil {
		t.Fatalf("runPrintMode returned error: %v", err)
	}
	if got := out.String(); got != "typed\n" {
		t.Fatalf("text output = %q, want typed newline", got)
	}
}

func TestPrintModeStreamsTextBeforePromptSettles(t *testing.T) {
	base := &printSession{events: make(chan agent.EventEnvelope, 3)}
	base.events <- printEnvelope(session.MessageUpdate{
		Delta:     session.TextDelta{Text: "partial"},
		BlockType: "text",
	})
	base.events <- printEnvelope(session.TurnEnd{Base: session.BaseNow()})
	base.events <- printEnvelope(session.Settled{})
	sess := &gatedPrintSession{printSession: base, release: make(chan struct{})}
	out := &signalWriter{firstWrite: make(chan struct{})}
	done := make(chan error, 1)
	go func() {
		done <- runPrintModeWithWriter(context.Background(), out, sess, "hello", "text")
	}()

	select {
	case <-out.firstWrite:
		if got := out.String(); got != "partial" {
			t.Fatalf("streamed output = %q, want partial before settlement", got)
		}
	case <-time.After(time.Second):
		t.Fatal("print mode did not stream text before Prompt settled")
	}
	close(sess.release)
	if err := <-done; err != nil {
		t.Fatalf("runPrintMode returned error: %v", err)
	}
	if got := out.String(); got != "partial\n" {
		t.Fatalf("final text output = %q, want partial newline", got)
	}
}

func TestPrintModeWritesIonEvents(t *testing.T) {
	sess := &printSession{events: make(chan agent.EventEnvelope, 5)}
	sess.events <- agent.EventEnvelope{
		Sequence: 7,
		Event: session.ToolExecStart{
			ToolCallID: "call-1",
			Name:       "read",
			Args:       []byte(`{"path":"README.md"}`),
		},
	}
	sess.events <- agent.EventEnvelope{
		Sequence: 8,
		Event: session.MessageUpdate{
			Delta:     session.TextDelta{Text: "done"},
			BlockType: "text",
		},
	}
	sess.events <- agent.EventEnvelope{
		Sequence: 9,
		Event: session.TurnEnd{
			Base:    session.BaseNow(),
			Message: &session.AssistantMessage{Content: []session.Content{session.TextContent{Text: "done"}}},
		},
	}
	sess.events <- agent.EventEnvelope{Event: session.Settled{}}

	var out bytes.Buffer
	if err := runPrintModeWithWriter(context.Background(), &out, sess, "hello", "events"); err != nil {
		t.Fatalf("runPrintMode returned error: %v", err)
	}

	var events []structuredPrintEvent
	decoder := json.NewDecoder(&out)
	for {
		var event structuredPrintEvent
		err := decoder.Decode(&event)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatalf("decode event stream %q: %v", out.String(), err)
		}
		events = append(events, event)
	}
	if len(events) != 5 {
		t.Fatalf("event count = %d, want 5: %s", len(events), out.String())
	}
	wantTypes := []string{"tool_start", "message_update", "turn_end", "settled", "result"}
	for i, want := range wantTypes {
		if events[i].Schema != printEventSchema || events[i].Index != uint64(i+1) || events[i].Type != want {
			t.Fatalf("event[%d] = %#v, want schema/index/type %q", i, events[i], want)
		}
	}
	if events[0].Sequence != 7 || events[1].Sequence != 8 || events[2].Sequence != 9 {
		t.Fatalf(
			"runtime sequences = %d, %d, %d; want 7, 8, 9",
			events[0].Sequence,
			events[1].Sequence,
			events[2].Sequence,
		)
	}
	var result printResult
	data, err := json.Marshal(events[4].Data)
	if err != nil {
		t.Fatalf("marshal result data: %v", err)
	}
	if err := json.Unmarshal(data, &result); err != nil {
		t.Fatalf("decode result data: %v", err)
	}
	if result.Response != "done" || !slices.Equal(result.ToolCalls, []string{"read"}) {
		t.Fatalf("result = %#v", result)
	}
}

func TestPrintModeWritesJSONOutput(t *testing.T) {
	sess := &printSession{events: make(chan agent.EventEnvelope, 5)}
	sess.events <- printEnvelope(session.ToolExecStart{Name: "read"})
	sess.events <- printEnvelope(session.MessageEnd{
		Message: &session.AssistantMessage{
			Content: []session.Content{session.TextContent{Text: "done"}},
			Usage:   session.Usage{Input: 12, Output: 3, Cost: session.Cost{Total: 0.25}},
		},
	})
	sess.events <- printEnvelope(session.TurnEnd{Base: session.BaseNow()})
	sess.events <- printEnvelope(session.Settled{})

	var out bytes.Buffer
	if err := runPrintModeWithWriter(context.Background(), &out, sess, "hello", "json"); err != nil {
		t.Fatalf("runPrintMode returned error: %v", err)
	}

	var result printResult
	if err := json.Unmarshal(out.Bytes(), &result); err != nil {
		t.Fatalf("decode json output %q: %v", out.String(), err)
	}
	if result.SessionID != "print-test" || result.Response != "done" ||
		result.InputTokens != 12 || result.OutputTokens != 3 || result.Cost != 0.25 ||
		len(result.ToolCalls) != 1 || result.ToolCalls[0] != "read" {
		t.Fatalf("json result = %#v", result)
	}
}

func TestPrintModeJSONAcceptanceCapturesStreamingToolAndUsage(t *testing.T) {
	sess := &printSession{events: make(chan agent.EventEnvelope, 7)}
	sess.events <- printEnvelope(session.TurnStart{Timestamp: time.Now()})
	sess.events <- printEnvelope(session.ToolExecStart{Name: "bash"})
	sess.events <- printEnvelope(session.MessageUpdate{
		Delta:     session.TextDelta{Text: "do"},
		BlockType: "text",
	})
	sess.events <- printEnvelope(session.MessageEnd{
		Message: &session.AssistantMessage{
			Usage: session.Usage{Input: 10, Output: 2, Cost: session.Cost{Total: 0.01}},
		},
	})
	sess.events <- printEnvelope(session.MessageUpdate{
		Delta:     session.TextDelta{Text: "ne"},
		BlockType: "text",
	})
	sess.events <- printEnvelope(session.TurnEnd{Base: session.BaseNow()})
	sess.events <- printEnvelope(session.Settled{})

	var out bytes.Buffer
	if err := runPrintModeWithWriter(context.Background(), &out, sess, "run smoke", "json"); err != nil {
		t.Fatalf("runPrintMode returned error: %v", err)
	}

	var result printResult
	if err := json.Unmarshal(out.Bytes(), &result); err != nil {
		t.Fatalf("decode json output %q: %v", out.String(), err)
	}
	if result.SessionID != "print-test" ||
		result.Response != "done" ||
		result.InputTokens != 10 ||
		result.OutputTokens != 2 ||
		result.Cost != 0.01 ||
		!slices.Equal(result.ToolCalls, []string{"bash"}) {
		t.Fatalf("json result = %#v", result)
	}
}

func TestPrintModeCancelsTurnOnTimeout(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	sess := &printSession{events: make(chan agent.EventEnvelope)}

	_, err := runPromptTurn(ctx, sess, "hello")
	if err == nil || !strings.Contains(err.Error(), "context canceled") {
		t.Fatalf("runPromptTurn error = %v, want context canceled", err)
	}
	if sess.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", sess.cancelled)
	}
}

func TestPrintModeWaitsForAcceptanceRaceSettlement(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	sess := &lateAcceptancePrintSession{
		printSession:      &printSession{events: make(chan agent.EventEnvelope, 1)},
		promptStarted:     make(chan struct{}),
		acceptanceRelease: make(chan struct{}),
		settledPublished:  make(chan struct{}),
	}
	done := make(chan error, 1)
	go func() {
		_, err := runPromptTurn(ctx, sess, "hello")
		done <- err
	}()

	select {
	case <-sess.promptStarted:
	case <-time.After(time.Second):
		t.Fatal("prompt did not start")
	}
	cancel()

	select {
	case err := <-done:
		if err == nil || !errors.Is(err, context.Canceled) {
			t.Fatalf("runPromptTurn error = %v, want context canceled", err)
		}
	case <-time.After(time.Second):
		t.Fatal("runPromptTurn did not settle")
	}
	select {
	case <-sess.settledPublished:
	default:
		t.Fatal("runPromptTurn returned before accepted turn settlement")
	}
}

func TestPrintModeTimeoutIsActionable(t *testing.T) {
	sess := &printSession{events: make(chan agent.EventEnvelope)}

	err := runPrintModeWithTimeout(
		context.Background(),
		&bytes.Buffer{},
		sess,
		"hello",
		time.Millisecond,
		"text",
	)
	if err == nil {
		t.Fatal("runPrintModeWithTimeout returned nil error")
	}
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("error = %v, want context deadline cause", err)
	}
	if got := err.Error(); got != "print mode timed out after 1ms" {
		t.Fatalf("timeout error = %q, want actionable print timeout", got)
	}
	if strings.Contains(err.Error(), "context deadline exceeded") {
		t.Fatalf("timeout error leaked raw context text: %q", err.Error())
	}
	if sess.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", sess.cancelled)
	}
}

func TestPrintModeReturnsSubmitError(t *testing.T) {
	sess := &printSession{submitErr: errors.New("provider unavailable")}

	_, err := runPromptTurn(context.Background(), sess, "hello")
	if err == nil || !strings.Contains(err.Error(), "submit turn: provider unavailable") {
		t.Fatalf("runPromptTurn error = %v, want submit error", err)
	}
}

func TestPrintModeReturnsSessionError(t *testing.T) {
	sess := &printSession{events: make(chan agent.EventEnvelope, 2)}
	sess.events <- printEnvelope(session.TurnEnd{Base: session.BaseNow(), Error: errors.New("rate limited")})
	sess.events <- printEnvelope(session.Settled{})

	_, err := runPromptTurn(context.Background(), sess, "hello")
	if err == nil || !strings.Contains(err.Error(), "session error: rate limited") {
		t.Fatalf("runPromptTurn error = %v, want session error", err)
	}
	if sess.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", sess.cancelled)
	}
}

func TestPrintModeReturnsSessionErrorFallback(t *testing.T) {
	sess := &printSession{events: make(chan agent.EventEnvelope, 2)}
	sess.events <- printEnvelope(session.TurnEnd{Base: session.BaseNow(), Error: errors.New("session error")})
	sess.events <- printEnvelope(session.Settled{})

	_, err := runPromptTurn(context.Background(), sess, "hello")
	if err == nil || !strings.Contains(err.Error(), "session error") {
		t.Fatalf("runPromptTurn error = %v, want fallback session error", err)
	}
	if sess.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", sess.cancelled)
	}
}

func TestPrintModeErrorsWhenEventStreamClosesBeforeTurnFinished(t *testing.T) {
	base := &printSession{events: make(chan agent.EventEnvelope, 1)}
	base.events <- printEnvelope(session.MessageUpdate{
		Delta:     session.TextDelta{Text: "partial"},
		BlockType: "text",
	})
	close(base.events)
	sess := &abortReleasingPrintSession{
		printSession:      base,
		unblock:           make(chan struct{}),
		abortCalled:       make(chan struct{}),
		globalAbortCalled: make(chan struct{}),
	}

	_, err := runPromptTurn(context.Background(), sess, "hello")
	if err == nil || !strings.Contains(err.Error(), "event stream closed before turn finished") {
		t.Fatalf("runPromptTurn error = %v, want early stream close error", err)
	}
	select {
	case <-sess.abortCalled:
	case <-time.After(time.Second):
		t.Fatal("canceled prompt did not receive scoped abort")
	}
	select {
	case <-sess.globalAbortCalled:
		t.Fatal("print mode used global Abort before prompt reservation")
	default:
	}
	if base.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", base.cancelled)
	}
}

func TestPrintModeAbortsBeforeWaitingForPromptOnClosedEventStream(t *testing.T) {
	base := &printSession{events: make(chan agent.EventEnvelope)}
	sess := &abortReleasingPrintSession{
		printSession:      base,
		unblock:           make(chan struct{}),
		abortCalled:       make(chan struct{}),
		globalAbortCalled: make(chan struct{}),
	}
	close(base.events)

	done := make(chan error, 1)
	go func() {
		_, err := runPromptTurn(context.Background(), sess, "hello")
		done <- err
	}()

	select {
	case err := <-done:
		if err == nil || !strings.Contains(err.Error(), "event stream closed before turn finished") {
			t.Fatalf("runPromptTurn error = %v, want early stream close error", err)
		}
	case <-time.After(time.Second):
		t.Fatal("runPromptTurn waited for Prompt before aborting the runner")
	}
	select {
	case <-sess.abortCalled:
	case <-time.After(time.Second):
		t.Fatal("canceled prompt did not receive scoped abort")
	}
	select {
	case <-sess.globalAbortCalled:
		t.Fatal("print mode used global Abort before prompt reservation")
	default:
	}
	if base.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", base.cancelled)
	}
}

func TestPrintModeErrorsWhenTurnFinishesWithoutAssistantResponse(t *testing.T) {
	sess := &printSession{events: make(chan agent.EventEnvelope, 2)}
	sess.events <- printEnvelope(session.TurnEnd{Base: session.BaseNow()})
	sess.events <- printEnvelope(session.Settled{})

	_, err := runPromptTurn(context.Background(), sess, "hello")
	if err == nil || !strings.Contains(err.Error(), "turn finished without assistant response") {
		t.Fatalf("runPromptTurn error = %v, want empty response error", err)
	}
}

func TestCloseRuntimeHandlesClosesPrintAgent(t *testing.T) {
	sess := &printSession{}

	if err := closeRuntimeHandles(sess, nil); err != nil {
		t.Fatalf("closeRuntimeHandles: %v", err)
	}
	if sess.closed != 1 {
		t.Fatalf("closed = %d, want 1", sess.closed)
	}
}

type nonCooperativeShutdownRuntime struct {
	*printSession
}

func (r *nonCooperativeShutdownRuntime) Shutdown(ctx context.Context) error {
	<-ctx.Done()
	return ctx.Err()
}

func (r *nonCooperativeShutdownRuntime) Close() error {
	r.closed++
	return nil
}

func TestCloseRuntimeHandlesWithContextDoesNotCloseDependenciesAfterTimeout(t *testing.T) {
	runtime := &nonCooperativeShutdownRuntime{printSession: &printSession{}}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()

	err := closeRuntimeHandlesWithContext(ctx, runtime, nil)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("closeRuntimeHandlesWithContext error = %v, want deadline exceeded", err)
	}
	if runtime.closed != 0 {
		t.Fatalf("runtime Close calls = %d, want 0 after bounded shutdown", runtime.closed)
	}
}

func TestPrintModeRejectsUnknownOutput(t *testing.T) {
	err := writePrintResult(&bytes.Buffer{}, printResult{Response: "x"}, "xml")
	if err == nil || !strings.Contains(err.Error(), "unsupported print output") {
		t.Fatalf("writePrintResult error = %v", err)
	}
}

func TestPromptWithStdinContextReadsStdinWhenPromptMissing(t *testing.T) {
	got := promptWithStdinContext("", "prompt from stdin\n")
	if got != "prompt from stdin\n" {
		t.Fatalf("promptWithStdinContext = %q, want stdin prompt", got)
	}
}

func TestPromptWithStdinContextReadsStdinForDashPrompt(t *testing.T) {
	got := promptWithStdinContext("-", "prompt from stdin\n")
	if got != "prompt from stdin\n" {
		t.Fatalf("promptWithStdinContext = %q, want stdin prompt", got)
	}
}

func TestPromptWithStdinContextAppendsNonEmptyStdin(t *testing.T) {
	got := promptWithStdinContext("summarize", "tool output\n")
	want := "summarize\n\n<stdin>\ntool output\n</stdin>"
	if got != want {
		t.Fatalf("promptWithStdinContext = %q, want %q", got, want)
	}
}

func TestPromptWithStdinContextIgnoresEmptyStdinWithPrompt(t *testing.T) {
	got := promptWithStdinContext("summarize", "\n\t ")
	if got != "summarize" {
		t.Fatalf("promptWithStdinContext = %q, want original prompt", got)
	}
}
