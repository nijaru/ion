package approval

import (
	"context"
	"errors"
	"sync"

	"github.com/nijaru/ion/internal/storage/session"
	"github.com/oklog/ulid/v2"
)

// Gate coordinates tool approval requests across automated policies and
// human-in-the-loop (HITL) resolution. It includes a circuit breaker that
// disables automated policies after N consecutive denials.
type Gate struct {
	policy    Policy
	audit     AuditLogger
	threshold int

	mu                    sync.Mutex
	pending               map[string]pendingRequest
	consecutiveDenials    int
	circuitBreakerTripped bool
}

type pendingRequest struct {
	ch       chan Result
	req      Request
	resolved bool
}

func NewGate(policy Policy) *Gate {
	return &Gate{
		policy:    policy,
		pending:   make(map[string]pendingRequest),
		threshold: 3, // Default circuit breaker threshold
	}
}

// WithThreshold configures the circuit breaker threshold.
func (m *Gate) WithThreshold(n int) *Gate {
	if m == nil {
		return nil
	}
	m.mu.Lock()
	m.threshold = n
	m.mu.Unlock()
	return m
}

// WithAuditLogger configures an append-only security audit logger.
func (m *Gate) WithAuditLogger(logger AuditLogger) *Gate {
	if m == nil {
		return nil
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	m.audit = logger
	return m
}

// ResetBreaker clears the circuit breaker state.
func (m *Gate) ResetBreaker() {
	if m == nil {
		return
	}
	m.mu.Lock()
	m.consecutiveDenials = 0
	m.circuitBreakerTripped = false
	m.mu.Unlock()
}

// IsTripped returns true if the circuit breaker has tripped.
func (m *Gate) IsTripped() bool {
	if m == nil {
		return false
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.circuitBreakerTripped
}

func (m *Gate) Request(
	ctx context.Context,
	sess *session.Session,
	toolName string,
	args string,
	requirement Requirement,
) (Result, error) {
	if sess == nil {
		return Result{}, errors.New("approval request: session is required")
	}
	req := Request{
		ID:        ulid.Make().String(),
		SessionID: sess.ID(),
		Tool:      toolName,
		Args:      args,
		Category:  requirement.Category,
		Operation: requirement.Operation,
		Resource:  requirement.Resource,
		Metadata:  cloneMetadata(requirement.Metadata),
	}

	if err := sess.Append(ctx, session.NewEvent(sess.ID(), session.ApprovalRequested, req)); err != nil {
		return Result{}, err
	}
	m.logAudit(context.Background(), AuditEvent{
		Kind:      AuditKindApprovalRequested,
		SessionID: sess.ID(),
		Tool:      toolName,
		Category:  requirement.Category,
		Operation: requirement.Operation,
		Resource:  requirement.Resource,
		Metadata:  cloneMetadata(requirement.Metadata),
	})

	m.mu.Lock()
	tripped := m.circuitBreakerTripped
	m.mu.Unlock()

	if m.policy != nil && !tripped {
		res, handled, err := m.policy.Decide(ctx, sess, req)
		if err != nil {
			cancelErr := m.appendCanceled(context.Background(), sess, req, err.Error())
			m.logAudit(context.Background(), AuditEvent{
				Kind:      AuditKindApprovalCanceled,
				SessionID: sess.ID(),
				Tool:      toolName,
				Category:  requirement.Category,
				Operation: requirement.Operation,
				Resource:  requirement.Resource,
				Metadata:  cloneMetadata(requirement.Metadata),
				Reason:    err.Error(),
			})
			return Result{}, errors.Join(err, cancelErr)
		}
		if handled {
			res.RequestID = req.ID
			res.Automated = true

			m.mu.Lock()
			if res.Decision == DecisionDeny {
				m.consecutiveDenials++
				if m.threshold > 0 && m.consecutiveDenials >= m.threshold {
					m.circuitBreakerTripped = true
				}
			} else {
				m.consecutiveDenials = 0
			}
			m.mu.Unlock()

			if err := m.appendResolved(ctx, sess, res); err != nil {
				return Result{}, err
			}
			m.logAudit(
				context.Background(),
				auditEventForApprovalResolution(sess.ID(), toolName, requirement, res),
			)
			return res, nil
		}
	}

	ch := make(chan Result, 1)
	m.mu.Lock()
	m.pending[req.ID] = pendingRequest{ch: ch, req: req}
	m.mu.Unlock()

	select {
	case res := <-ch:
		if err := m.appendResolved(ctx, sess, res); err != nil {
			return Result{}, err
		}
		return res, nil
	case <-ctx.Done():
		m.mu.Lock()
		delete(m.pending, req.ID)
		m.mu.Unlock()
		_ = m.appendCanceled(context.Background(), sess, req, ctx.Err().Error())
		m.logAudit(context.Background(), AuditEvent{
			Kind:      AuditKindApprovalCanceled,
			SessionID: sess.ID(),
			Tool:      toolName,
			Category:  requirement.Category,
			Operation: requirement.Operation,
			Resource:  requirement.Resource,
			Metadata:  cloneMetadata(requirement.Metadata),
			Reason:    ctx.Err().Error(),
		})
		return Result{}, ctx.Err()
	}
}

func (m *Gate) Resolve(requestID string, decision Decision, reason string) error {
	if decision != DecisionAllow && decision != DecisionDeny {
		return ErrInvalidDecision
	}

	m.mu.Lock()
	pending, ok := m.pending[requestID]
	if !ok {
		m.mu.Unlock()
		return ErrRequestNotFound
	}
	if pending.resolved {
		m.mu.Unlock()
		return ErrRequestResolved
	}
	pending.resolved = true
	m.pending[requestID] = pending
	delete(m.pending, requestID)

	if decision == DecisionAllow {
		m.consecutiveDenials = 0
		m.circuitBreakerTripped = false
	}

	pending.ch <- Result{
		RequestID: requestID,
		Decision:  decision,
		Reason:    reason,
	}
	kind := AuditKindToolAllowed
	if decision == DecisionDeny {
		kind = AuditKindToolDenied
	}
	event := AuditEvent{
		Kind:      kind,
		SessionID: pending.req.SessionID,
		Tool:      pending.req.Tool,
		Category:  pending.req.Category,
		Operation: pending.req.Operation,
		Resource:  pending.req.Resource,
		Decision:  string(decision),
		Reason:    reason,
		Metadata:  cloneMetadata(pending.req.Metadata),
	}
	m.mu.Unlock()

	m.logAudit(context.Background(), event)
	return nil
}

func (m *Gate) Pending() []string {
	m.mu.Lock()
	defer m.mu.Unlock()
	ids := make([]string, 0, len(m.pending))
	for id := range m.pending {
		ids = append(ids, id)
	}
	return ids
}

func (m *Gate) appendResolved(ctx context.Context, sess *session.Session, result Result) error {
	return sess.Append(ctx, session.NewEvent(sess.ID(), session.ApprovalResolved, map[string]any{
		"id":       result.RequestID,
		"decision": result.Decision,
		"reason":   result.Reason,
	}))
}

func (m *Gate) appendCanceled(
	ctx context.Context,
	sess *session.Session,
	req Request,
	reason string,
) error {
	return sess.Append(ctx, session.NewEvent(sess.ID(), session.ApprovalCanceled, map[string]any{
		"id":     req.ID,
		"tool":   req.Tool,
		"reason": reason,
	}))
}
