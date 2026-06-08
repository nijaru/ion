package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

type printSession struct {
	events    chan session.AgentEvent
	cancelled int
	closed    int
	submitErr error
}

func (s *printSession) Open(ctx context.Context) error              { return nil }
func (s *printSession) Resume(ctx context.Context, id string) error { return nil }
func (s *printSession) SubmitTurn(ctx context.Context, turn string) error {
	if s.submitErr != nil {
		return s.submitErr
	}
	return nil
}

func (s *printSession) CancelTurn(ctx context.Context) error {
	s.cancelled++
	return nil
}

func (s *printSession) Close() error {
	s.closed++
	return nil
}
func (s *printSession) Events() <-chan session.AgentEvent { return s.events }
func (s *printSession) ID() string                        { return "print-test" }
func (s *printSession) Meta() map[string]string           { return nil }

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
}

func TestPrintModeRejectsUnexpectedApprovalRequest(t *testing.T) {
	sess := &printSession{events: make(chan session.AgentEvent, 1)}
	sess.events <- session.ApprovalRequest{RequestID: "req-1", ToolName: "bash"}

	err := runPrintMode(context.Background(), sess, "hello")
	if err == nil {
		t.Fatal("runPrintMode returned nil error")
	}
	if err.Error() != "unexpected approval request for bash" {
		t.Fatalf("runPrintMode error = %v", err)
	}
	if sess.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", sess.cancelled)
	}
}

func TestPrintModeWritesTextOutput(t *testing.T) {
	sess := &printSession{events: make(chan session.AgentEvent, 3)}
	sess.events <- session.AgentDelta{Delta: "hello"}
	sess.events <- session.AgentDelta{Delta: " world"}
	sess.events <- session.TurnEnd{}

	var out bytes.Buffer
	if err := runPrintModeWithWriter(context.Background(), &out, sess, "hello", "text"); err != nil {
		t.Fatalf("runPrintMode returned error: %v", err)
	}
	if got := out.String(); got != "hello world\n" {
		t.Fatalf("text output = %q, want hello world newline", got)
	}
}

func TestPrintModeWritesJSONOutput(t *testing.T) {
	sess := &printSession{events: make(chan session.AgentEvent, 4)}
	sess.events <- session.ToolCallStart{ToolName: "read"}
	sess.events <- session.AgentMessage{InputTokens: 12, OutputTokens: 3, Cost: 0.25}
	sess.events <- session.AgentMessage{Message: "done"}
	sess.events <- session.TurnEnd{}

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
	sess := &printSession{events: make(chan session.AgentEvent, 6)}
	sess.events <- session.TurnStart{}
	sess.events <- session.ToolCallStart{ToolName: "bash"}
	sess.events <- session.AgentDelta{Delta: "do"}
	sess.events <- session.AgentMessage{InputTokens: 10, OutputTokens: 2, Cost: 0.01}
	sess.events <- session.AgentDelta{Delta: "ne"}
	sess.events <- session.TurnEnd{}

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
	sess := &printSession{events: make(chan session.AgentEvent)}

	_, err := runPromptTurn(ctx, sess, "hello")
	if err == nil || !strings.Contains(err.Error(), "context canceled") {
		t.Fatalf("runPromptTurn error = %v, want context canceled", err)
	}
	if sess.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", sess.cancelled)
	}
}

func TestPrintModeTimeoutIsActionable(t *testing.T) {
	sess := &printSession{events: make(chan session.AgentEvent)}

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
	sess := &printSession{events: make(chan session.AgentEvent, 1)}
	sess.events <- session.TurnError{Err: errors.New("rate limited")}

	_, err := runPromptTurn(context.Background(), sess, "hello")
	if err == nil || !strings.Contains(err.Error(), "session error: rate limited") {
		t.Fatalf("runPromptTurn error = %v, want session error", err)
	}
	if sess.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", sess.cancelled)
	}
}

func TestPrintModeReturnsSessionErrorFallback(t *testing.T) {
	sess := &printSession{events: make(chan session.AgentEvent, 1)}
	sess.events <- session.TurnError{}

	_, err := runPromptTurn(context.Background(), sess, "hello")
	if err == nil || err.Error() != "session error" {
		t.Fatalf("runPromptTurn error = %v, want fallback session error", err)
	}
	if sess.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", sess.cancelled)
	}
}

func TestPrintModeErrorsWhenEventStreamClosesBeforeTurnFinished(t *testing.T) {
	sess := &printSession{events: make(chan session.AgentEvent, 1)}
	sess.events <- session.AgentDelta{Delta: "partial"}
	close(sess.events)

	_, err := runPromptTurn(context.Background(), sess, "hello")
	if err == nil || !strings.Contains(err.Error(), "event stream closed before turn finished") {
		t.Fatalf("runPromptTurn error = %v, want early stream close error", err)
	}
	if sess.cancelled != 1 {
		t.Fatalf("cancelled = %d, want 1", sess.cancelled)
	}
}

func TestPrintModeErrorsWhenTurnFinishesWithoutAssistantResponse(t *testing.T) {
	sess := &printSession{events: make(chan session.AgentEvent, 1)}
	sess.events <- session.TurnEnd{}

	_, err := runPromptTurn(context.Background(), sess, "hello")
	if err == nil || !strings.Contains(err.Error(), "turn finished without assistant response") {
		t.Fatalf("runPromptTurn error = %v, want empty response error", err)
	}
}

func TestCloseRuntimeHandlesClosesPrintAgent(t *testing.T) {
	sess := &printSession{}

	if err := closeRuntimeHandles(sess, nil, nil); err != nil {
		t.Fatalf("closeRuntimeHandles: %v", err)
	}
	if sess.closed != 1 {
		t.Fatalf("closed = %d, want 1", sess.closed)
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
