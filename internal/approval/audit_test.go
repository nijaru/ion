package approval

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/storage/session"
)

func TestManager_LogsAuditEvents(t *testing.T) {
	logger := &recordingAuditLogger{}
	mgr := NewGate(nil).WithAuditLogger(logger)
	sess := session.New("audit")

	done := make(chan Result, 1)
	go func() {
		res, err := mgr.Request(context.Background(), sess, "bash", "{}", Requirement{
			Category:  "command",
			Operation: "exec",
			Resource:  "bash",
		})
		if err != nil {
			t.Errorf("Request: %v", err)
			return
		}
		done <- res
	}()

	time.Sleep(10 * time.Millisecond)
	pending := mgr.Pending()
	if len(pending) != 1 {
		t.Fatalf("expected 1 pending request, got %d", len(pending))
	}
	if err := mgr.Resolve(pending[0], DecisionAllow, "ok"); err != nil {
		t.Fatalf("Resolve: %v", err)
	}

	select {
	case res := <-done:
		if !res.Allowed() {
			t.Fatalf("expected allowed result, got %#v", res)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for resolution")
	}

	events := logger.Events()
	if len(events) != 2 {
		t.Fatalf("expected 2 audit events, got %d", len(events))
	}
	if events[0].Kind != AuditKindApprovalRequested {
		t.Fatalf("first audit kind = %q, want %q", events[0].Kind, AuditKindApprovalRequested)
	}
	if events[1].Kind != AuditKindToolAllowed {
		t.Fatalf("second audit kind = %q, want %q", events[1].Kind, AuditKindToolAllowed)
	}
}

type recordingAuditLogger struct {
	mu     sync.Mutex
	events []AuditEvent
}

func (l *recordingAuditLogger) Log(_ context.Context, event AuditEvent) error {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.events = append(l.events, event)
	return nil
}

func (l *recordingAuditLogger) Events() []AuditEvent {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]AuditEvent(nil), l.events...)
}
