package main

import (
	"context"
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
