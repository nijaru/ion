package main

import (
	"bytes"
	"context"
	"encoding/json"
	"slices"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/session"
)

type printSession struct {
	events      chan session.Event
	mode        session.Mode
	autoApprove bool
	approved    bool
}

func (s *printSession) Open(ctx context.Context) error              { return nil }
func (s *printSession) Resume(ctx context.Context, id string) error { return nil }
func (s *printSession) SubmitTurn(ctx context.Context, turn string) error {
	return nil
}
func (s *printSession) CancelTurn(ctx context.Context) error { return nil }
func (s *printSession) Approve(ctx context.Context, requestID string, approved bool) error {
	s.approved = approved
	return nil
}
func (s *printSession) RegisterMCPServer(ctx context.Context, cmd string, args ...string) error {
	return nil
}
func (s *printSession) SetMode(mode session.Mode)     { s.mode = mode }
func (s *printSession) SetAutoApprove(enabled bool)   { s.autoApprove = enabled }
func (s *printSession) AllowCategory(toolName string) {}
func (s *printSession) Close() error                  { return nil }
func (s *printSession) Events() <-chan session.Event  { return s.events }
func (s *printSession) ID() string                    { return "print-test" }
func (s *printSession) Meta() map[string]string       { return nil }

func TestConfigureSessionMode(t *testing.T) {
	sess := &printSession{}

	configureSessionMode(sess, session.ModeRead)
	if sess.mode != session.ModeRead {
		t.Fatalf("mode = %v, want read", sess.mode)
	}
	if sess.autoApprove {
		t.Fatal("read mode enabled auto approval")
	}

	configureSessionMode(sess, session.ModeYolo)
	if sess.mode != session.ModeYolo {
		t.Fatalf("mode = %v, want auto", sess.mode)
	}
	if !sess.autoApprove {
		t.Fatal("auto mode did not enable auto approval")
	}
}

func TestResolvePrintFlagsSupportsShortPrintWithPositionalPrompt(t *testing.T) {
	requested, prompt, output, err := resolvePrintFlags(false, true, "", []string{"hello"}, "text", false)
	if err != nil {
		t.Fatalf("resolve print flags: %v", err)
	}
	if !requested || prompt != "hello" || output != "text" {
		t.Fatalf("requested=%v prompt=%q output=%q, want print hello text", requested, prompt, output)
	}
}

func TestResolvePrintFlagsSupportsJSONShortcut(t *testing.T) {
	requested, prompt, output, err := resolvePrintFlags(false, false, "hello", nil, "text", true)
	if err != nil {
		t.Fatalf("resolve print flags: %v", err)
	}
	if !requested || prompt != "hello" || output != "json" {
		t.Fatalf("requested=%v prompt=%q output=%q, want print hello json", requested, prompt, output)
	}
}

func TestResolvePrintFlagsUsesPositionalPromptInPrintMode(t *testing.T) {
	requested, prompt, output, err := resolvePrintFlags(true, false, "", []string{"hello", "world"}, "", false)
	if err != nil {
		t.Fatalf("resolve print flags: %v", err)
	}
	if !requested || prompt != "hello world" || output != "text" {
		t.Fatalf("requested=%v prompt=%q output=%q, want joined positional prompt", requested, prompt, output)
	}
}

func TestResolvePrintFlagsRejectsUnexpectedArguments(t *testing.T) {
	_, _, _, err := resolvePrintFlags(false, false, "", []string{"hello"}, "text", false)
	if err == nil || !strings.Contains(err.Error(), "unexpected arguments") {
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

func TestPrintModeRejectsApprovalWhenNotAutoApproved(t *testing.T) {
	sess := &printSession{events: make(chan session.Event, 1)}
	sess.events <- session.ApprovalRequest{RequestID: "req-1", ToolName: "bash"}

	err := runPrintMode(context.Background(), sess, "hello", false)
	if err == nil {
		t.Fatal("runPrintMode returned nil error")
	}
	if err.Error() != "approval required for bash" {
		t.Fatalf("runPrintMode error = %v", err)
	}
	if sess.approved {
		t.Fatal("approval was sent despite approveRequests=false")
	}
}

func TestPrintModeApprovesWhenAutoApproved(t *testing.T) {
	sess := &printSession{events: make(chan session.Event, 2)}
	sess.events <- session.ApprovalRequest{RequestID: "req-1", ToolName: "bash"}
	sess.events <- session.TurnFinished{}

	if err := runPrintMode(context.Background(), sess, "hello", true); err != nil {
		t.Fatalf("runPrintMode returned error: %v", err)
	}
	if !sess.approved {
		t.Fatal("approval was not sent")
	}
}

func TestPrintModeWritesTextOutput(t *testing.T) {
	sess := &printSession{events: make(chan session.Event, 3)}
	sess.events <- session.AgentDelta{Delta: "hello"}
	sess.events <- session.AgentDelta{Delta: " world"}
	sess.events <- session.TurnFinished{}

	var out bytes.Buffer
	if err := runPrintModeWithWriter(context.Background(), &out, sess, "hello", false, "text"); err != nil {
		t.Fatalf("runPrintMode returned error: %v", err)
	}
	if got := out.String(); got != "hello world\n" {
		t.Fatalf("text output = %q, want hello world newline", got)
	}
}

func TestPrintModeWritesJSONOutput(t *testing.T) {
	sess := &printSession{events: make(chan session.Event, 4)}
	sess.events <- session.ToolCallStarted{ToolName: "read"}
	sess.events <- session.TokenUsage{Input: 12, Output: 3, Cost: 0.25}
	sess.events <- session.AgentMessage{Message: "done"}
	sess.events <- session.TurnFinished{}

	var out bytes.Buffer
	if err := runPrintModeWithWriter(context.Background(), &out, sess, "hello", false, "json"); err != nil {
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
