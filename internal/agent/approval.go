package agent

import (
	"context"
	"errors"
	"fmt"
	"sort"
	"sync"
	"time"

	"github.com/nijaru/ion/session"
)

// ApprovalMode controls whether requirement-bearing tools can execute without
// a host decision. The runtime boundary records the exact action before this
// broker is consulted; this broker only handles the host decision protocol.
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
	order   uint64
}

// ApprovalBroker owns the pending-decision protocol for one Controller. It is
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
	switch mode {
	case ApprovalConfirm, ApprovalTrusted:
	default:
		// Constructors are an independent safety boundary; do not rely on the
		// host config normalizer to protect callers that build a controller
		// directly.
		mode = ApprovalConfirm
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
	if req.Fingerprint != "" {
		return req.Fingerprint
	}
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
	req = cloneApprovalRequest(req)
	req.ID = fmt.Sprintf("approval-%d", b.nextID)
	if req.Timestamp.IsZero() {
		req.Timestamp = time.Now()
	}
	pending := pendingApproval{request: req, result: make(chan approvalOutcome, 1), order: b.nextID}
	b.pending[req.ID] = pending
	// Publish while holding the broker lock. Snapshot capture takes the same
	// lock, so a request cannot appear in a resync without its event already
	// having an ordered place in the runtime stream. Keep the event detached
	// from the broker-owned request slices as well.
	b.emitEvent(cloneApprovalRequest(req))
	b.mu.Unlock()
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

// snapshot returns every currently pending request in deterministic ID order.
// The broker remains the authority; callers receive detached request values so
// resync cannot expose mutable broker-owned slices.
func (b *ApprovalBroker) snapshot() []session.ApprovalRequest {
	if b == nil {
		return nil
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	type orderedRequest struct {
		request session.ApprovalRequest
		order   uint64
	}
	ordered := make([]orderedRequest, 0, len(b.pending))
	for _, pending := range b.pending {
		ordered = append(ordered, orderedRequest{
			request: cloneApprovalRequest(pending.request),
			order:   pending.order,
		})
	}
	sort.Slice(ordered, func(i, j int) bool { return ordered[i].order < ordered[j].order })
	requests := make([]session.ApprovalRequest, 0, len(ordered))
	for _, pending := range ordered {
		requests = append(requests, pending.request)
	}
	return requests
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
	// Publish before waking the tool worker so the lifecycle stream records the
	// resolution before any resumed tool/execution events.
	b.emitEvent(session.ApprovalResolution{
		ID:        id,
		Decision:  outcome.decision,
		Reason:    outcome.reason,
		Timestamp: time.Now(),
	})
	pending.result <- outcome
	b.mu.Unlock()
	return true
}

func cloneApprovalRequest(req session.ApprovalRequest) session.ApprovalRequest {
	req.Paths = append([]string(nil), req.Paths...)
	return req
}

func (b *ApprovalBroker) emitEvent(event session.Event) {
	if b != nil && b.emit != nil {
		b.emit(event)
	}
}
