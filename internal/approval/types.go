package approval

import (
	"context"
	"errors"
	"fmt"

	"github.com/nijaru/ion/internal/storage/session"
)

type Decision string

const (
	DecisionAllow Decision = "allow"
	DecisionDeny  Decision = "deny"
)

const (
	AuditKindApprovalRequested = "security.approval.requested"
	AuditKindApprovalResolved  = "security.approval.resolved"
	AuditKindApprovalCanceled  = "security.approval.canceled"
	AuditKindToolAllowed       = "security.tool.allowed"
	AuditKindToolDenied        = "security.tool.denied"
)

var (
	ErrRequestNotFound = errors.New("approval request not found")
	ErrRequestResolved = errors.New("approval request already resolved")
	ErrInvalidDecision = errors.New("invalid approval decision")
)

type Requirement struct {
	Category  string
	Operation string
	Resource  string
	Metadata  map[string]any
}

// RequirementProvider is implemented by tools that can declare an approval
// requirement for a provider-supplied argument payload.
type RequirementProvider interface {
	ApprovalRequirement(args string) (Requirement, bool, error)
}

type Request struct {
	ID        string
	SessionID string
	Tool      string
	Args      string
	Category  string
	Operation string
	Resource  string
	Metadata  map[string]any
}

type Result struct {
	RequestID string
	Decision  Decision
	Reason    string
	Automated bool // true if decided by policy, false if resolved via Resolve (HITL)
}

type Policy interface {
	Decide(ctx context.Context, sess *session.Session, req Request) (Result, bool, error)
}

// AuditEvent is one approval lifecycle fact emitted by Gate when an audit
// logger is configured.
type AuditEvent struct {
	Kind      string
	SessionID string
	Tool      string
	Category  string
	Operation string
	Resource  string
	Decision  string
	Reason    string
	Metadata  map[string]any
}

// AuditLogger appends approval audit facts. Use package approvalaudit to adapt
// these facts to the generic audit package.
type AuditLogger interface {
	Log(ctx context.Context, event AuditEvent) error
}

func (r Result) Allowed() bool {
	return r.Decision == DecisionAllow
}

func (r Result) Error() error {
	if r.Decision == DecisionDeny {
		if r.Reason == "" {
			return fmt.Errorf("approval denied")
		}
		return fmt.Errorf("approval denied: %s", r.Reason)
	}
	return nil
}
