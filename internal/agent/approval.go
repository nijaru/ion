package agent

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/nijaru/ion/session"
)

// ApprovalMode controls whether requirement-bearing tools can execute without
// a host decision. Trusted is the default for backwards-safe local operation;
// Confirm is fail-closed when no interactive host is attached.
type ApprovalMode string

const (
	ApprovalTrusted ApprovalMode = "trusted"
	ApprovalConfirm ApprovalMode = "confirm"
)

type approvalOutcome struct {
	decision session.ApprovalDecision
	reason   string
}

type pendingApproval struct {
	request session.ApprovalRequest
	result  chan approvalOutcome
}

// ApprovalBroker owns the pending-decision protocol for one Harness. It is
// deliberately runtime-only: the durable result is the tool error/success
// message emitted after the decision.
type ApprovalBroker struct {
	mode        ApprovalMode
	interactive bool
	emit        func(session.Event)
	done        chan struct{}

	mu      sync.Mutex
	nextID  uint64
	pending map[string]pendingApproval
	always  map[string]struct{}
	closed  bool
}

func NewApprovalBroker(
	mode ApprovalMode,
	interactive bool,
	emit func(session.Event),
) *ApprovalBroker {
	if mode != ApprovalConfirm {
		mode = ApprovalTrusted
	}
	return &ApprovalBroker{
		mode:        mode,
		interactive: interactive,
		emit:        emit,
		done:        make(chan struct{}),
		pending:     make(map[string]pendingApproval),
		always:      make(map[string]struct{}),
	}
}

func approvalKey(req session.ApprovalRequest) string {
	return req.ToolName + "\x00" + req.Operation + "\x00" + req.Resource
}

// Request blocks until the host resolves req, the turn is canceled, or the
// broker closes. It always returns a concrete decision so callers fail closed.
func (b *ApprovalBroker) Request(ctx context.Context, req session.ApprovalRequest) approvalOutcome {
	return b.request(ctx, req, false)
}

// RequestForced applies the interactive approval boundary even when the
// runtime's default mode is trusted. Runtime-scoped "always" decisions still
// apply after the host explicitly grants one.
func (b *ApprovalBroker) RequestForced(ctx context.Context, req session.ApprovalRequest) approvalOutcome {
	return b.request(ctx, req, true)
}

func (b *ApprovalBroker) request(ctx context.Context, req session.ApprovalRequest, forced bool) approvalOutcome {
	if ctx == nil {
		ctx = context.Background()
	}
	if b == nil || (!forced && b.mode != ApprovalConfirm) {
		return approvalOutcome{decision: session.ApprovalAllow}
	}

	key := approvalKey(req)
	b.mu.Lock()
	if b.closed {
		b.mu.Unlock()
		return approvalOutcome{
			decision: session.ApprovalDeny,
			reason:   "tool approval is unavailable in this runtime",
		}
	}
	if _, ok := b.always[key]; ok {
		b.mu.Unlock()
		return approvalOutcome{decision: session.ApprovalAlways}
	}
	if !b.interactive {
		b.mu.Unlock()
		return approvalOutcome{
			decision: session.ApprovalDeny,
			reason:   "tool approval is unavailable in this runtime",
		}
	}
	b.nextID++
	req.ID = fmt.Sprintf("approval-%d", b.nextID)
	if req.Timestamp.IsZero() {
		req.Timestamp = time.Now()
	}
	pending := pendingApproval{request: req, result: make(chan approvalOutcome, 1)}
	b.pending[req.ID] = pending
	b.mu.Unlock()

	b.emitEvent(req)
	select {
	case outcome := <-pending.result:
		return outcome
	case <-ctx.Done():
		b.finish(req.ID, approvalOutcome{
			decision: session.ApprovalDeny,
			reason:   "tool approval canceled",
		})
		return <-pending.result
	case <-b.done:
		b.finish(req.ID, approvalOutcome{
			decision: session.ApprovalDeny,
			reason:   "tool approval canceled during shutdown",
		})
		return <-pending.result
	}
}

// Resolve supplies one host decision. Duplicate or unknown IDs are errors;
// this makes exactly-once resolution observable to the caller.
func (b *ApprovalBroker) Resolve(id string, decision session.ApprovalDecision) error {
	if b == nil {
		return errors.New("approval broker is unavailable")
	}
	switch decision {
	case session.ApprovalAllow, session.ApprovalDeny, session.ApprovalAlways:
	default:
		return fmt.Errorf("unknown approval decision %q", decision)
	}
	b.mu.Lock()
	_, ok := b.pending[id]
	b.mu.Unlock()
	if !ok {
		return fmt.Errorf("approval request %q is no longer pending", id)
	}
	reason := ""
	if decision == session.ApprovalDeny {
		reason = "tool call denied by user"
	}
	if !b.finish(id, approvalOutcome{decision: decision, reason: reason}) {
		return fmt.Errorf("approval request %q is no longer pending", id)
	}
	return nil
}

// Close denies every pending request and prevents new interactive requests.
func (b *ApprovalBroker) Close() error {
	if b == nil {
		return nil
	}
	b.mu.Lock()
	if b.closed {
		b.mu.Unlock()
		return nil
	}
	b.closed = true
	close(b.done)
	ids := make([]string, 0, len(b.pending))
	for id := range b.pending {
		ids = append(ids, id)
	}
	b.mu.Unlock()
	for _, id := range ids {
		b.finish(id, approvalOutcome{
			decision: session.ApprovalDeny,
			reason:   "tool approval canceled during shutdown",
		})
	}
	return nil
}

func (b *ApprovalBroker) finish(id string, outcome approvalOutcome) bool {
	b.mu.Lock()
	pending, ok := b.pending[id]
	if !ok {
		b.mu.Unlock()
		return false
	}
	delete(b.pending, id)
	if outcome.decision == session.ApprovalAlways {
		b.always[approvalKey(pending.request)] = struct{}{}
	}
	pending.result <- outcome
	b.mu.Unlock()

	b.emitEvent(session.ApprovalResolution{
		ID:        id,
		Decision:  outcome.decision,
		Reason:    outcome.reason,
		Timestamp: time.Now(),
	})
	return true
}

func (b *ApprovalBroker) emitEvent(event session.Event) {
	if b != nil && b.emit != nil {
		b.emit(event)
	}
}
