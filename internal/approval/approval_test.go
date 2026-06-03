package approval

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestManager_RequestResolveAllow(t *testing.T) {
	mgr := NewGate(nil)
	sess := session.New("allow")

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
}

func TestManager_RequestResolveDeny(t *testing.T) {
	mgr := NewGate(nil)
	sess := session.New("deny")

	done := make(chan error, 1)
	go func() {
		res, err := mgr.Request(context.Background(), sess, "write_file", "{}", Requirement{
			Category:  "workspace",
			Operation: "write_file",
			Resource:  "a.txt",
		})
		if err != nil {
			done <- err
			return
		}
		done <- res.Error()
	}()

	time.Sleep(10 * time.Millisecond)
	pending := mgr.Pending()
	if len(pending) != 1 {
		t.Fatalf("expected 1 pending request, got %d", len(pending))
	}
	if err := mgr.Resolve(pending[0], DecisionDeny, "unsafe"); err != nil {
		t.Fatalf("Resolve: %v", err)
	}

	select {
	case err := <-done:
		if err == nil || err.Error() != "approval denied: unsafe" {
			t.Fatalf("unexpected deny error: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for denial")
	}
}

func TestManager_DuplicateAndLateDecisionRejected(t *testing.T) {
	mgr := NewGate(nil)
	sess := session.New("dup")

	go func() {
		_, _ = mgr.Request(context.Background(), sess, "bash", "{}", Requirement{
			Category: "command", Operation: "exec",
		})
	}()

	time.Sleep(10 * time.Millisecond)
	pending := mgr.Pending()
	if len(pending) != 1 {
		t.Fatalf("expected 1 pending request, got %d", len(pending))
	}
	id := pending[0]
	if err := mgr.Resolve(id, DecisionAllow, "ok"); err != nil {
		t.Fatalf("first Resolve: %v", err)
	}
	if err := mgr.Resolve(id, DecisionAllow, "again"); !errors.Is(err, ErrRequestNotFound) {
		t.Fatalf("second Resolve = %v, want ErrRequestNotFound", err)
	}
}

func TestManager_RequestCancellation(t *testing.T) {
	mgr := NewGate(nil)
	sess := session.New("cancel")

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		_, err := mgr.Request(ctx, sess, "bash", "{}", Requirement{
			Category: "command", Operation: "exec",
		})
		done <- err
	}()

	time.Sleep(10 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("expected context.Canceled, got %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for cancellation")
	}
	if got := len(mgr.Pending()); got != 0 {
		t.Fatalf("expected no pending requests after cancel, got %d", got)
	}
}

func TestManager_PolicyErrorClearsWaitingState(t *testing.T) {
	mgr := NewGate(errorPolicy{err: errors.New("classifier failed")})
	sess := session.New("policy-error")

	_, err := mgr.Request(t.Context(), sess, "bash", "{}", Requirement{
		Category:  "command",
		Operation: "exec",
		Resource:  "bash",
	})
	if err == nil || !strings.Contains(err.Error(), "classifier failed") {
		t.Fatalf("expected policy error, got %v", err)
	}
	if sess.IsWaiting() {
		t.Fatal("policy error left session waiting")
	}
	if pending := mgr.Pending(); len(pending) != 0 {
		t.Fatalf("policy error left pending approvals: %#v", pending)
	}
	last, ok := sess.LastEvent()
	if !ok || last.Type != session.ApprovalCanceled {
		t.Fatalf("last event = %#v, want approval canceled", last)
	}
}

func TestManager_CircuitBreaker(t *testing.T) {
	policy := &mockDenyPolicy{}
	mgr := NewGate(policy).WithThreshold(2)
	sess := session.New("breaker")

	// 1. First automated denial
	res, err := mgr.Request(context.Background(), sess, "t1", "{}", Requirement{Category: "cat"})
	if err != nil {
		t.Fatalf("Request 1: %v", err)
	}
	if res.Decision != DecisionDeny || !res.Automated {
		t.Fatalf("Request 1 result = %+v", res)
	}

	// 2. Second automated denial (should trip the breaker)
	res, err = mgr.Request(context.Background(), sess, "t2", "{}", Requirement{Category: "cat"})
	if err != nil {
		t.Fatalf("Request 2: %v", err)
	}
	if res.Decision != DecisionDeny || !res.Automated {
		t.Fatalf("Request 2 result = %+v", res)
	}

	// 3. Third request should bypass policy and wait for HITL
	done := make(chan Result, 1)
	go func() {
		r, e := mgr.Request(context.Background(), sess, "t3", "{}", Requirement{Category: "cat"})
		if e != nil {
			t.Errorf("Request 3 error: %v", e)
			return
		}
		done <- r
	}()

	time.Sleep(10 * time.Millisecond)
	pending := mgr.Pending()
	if len(pending) != 1 {
		t.Fatalf("Request 3 should be pending HITL after breaker trip, got %d", len(pending))
	}

	// 4. Manual approval should reset the breaker
	if err := mgr.Resolve(pending[0], DecisionAllow, "human override"); err != nil {
		t.Fatalf("Resolve: %v", err)
	}

	res = <-done
	if res.Decision != DecisionAllow || res.Automated {
		t.Fatalf("Request 3 resolution = %+v", res)
	}

	// 5. Next request should use automated policy again
	res, err = mgr.Request(context.Background(), sess, "t4", "{}", Requirement{Category: "cat"})
	if err != nil {
		t.Fatalf("Request 4: %v", err)
	}
	if res.Decision != DecisionDeny || !res.Automated {
		t.Fatalf("Request 4 result = %+v (should be automated denial)", res)
	}
}

type mockDenyPolicy struct{}

func (p *mockDenyPolicy) Decide(
	ctx context.Context,
	sess *session.Session,
	req Request,
) (Result, bool, error) {
	return Result{Decision: DecisionDeny, Reason: "automated block"}, true, nil
}

type errorPolicy struct {
	err error
}

func (p errorPolicy) Decide(
	ctx context.Context,
	sess *session.Session,
	req Request,
) (Result, bool, error) {
	return Result{}, false, p.err
}
