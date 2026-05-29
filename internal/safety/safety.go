package safety

import (
	"context"

	"github.com/nijaru/ion/internal/approval"
	"github.com/nijaru/ion/internal/audit"
	"github.com/nijaru/ion/internal/storage/session"
)

// Mode defines the execution mode of the agent.
type Mode string

const (
	// ModeRead allows only read-only operations.
	ModeRead Mode = "read"
	// ModeEdit requires approval for write and execute operations.
	ModeEdit Mode = "edit"
	// ModeAuto allows all operations without approval.
	ModeAuto Mode = "auto"
)

// Category defines the type of operation a tool performs.
type Category string

const (
	CategoryRead    Category = "read"
	CategoryWrite   Category = "write"
	CategoryExecute Category = "execute"
)

// Config is an approval.Policy implementation that enforces safety modes.
type Config struct {
	mode  Mode
	audit audit.Logger
}

// NewConfig creates a new safety config with the given mode.
func NewConfig(mode Mode) *Config {
	return &Config{mode: mode}
}

// WithAuditLogger configures append-only security logging for policy decisions.
func (p *Config) WithAuditLogger(logger audit.Logger) *Config {
	if p == nil {
		return nil
	}
	p.audit = logger
	return p
}

// Decide implements approval.Policy.
func (p *Config) Decide(
	ctx context.Context,
	sess *session.Session,
	req approval.Request,
) (approval.Result, bool, error) {
	switch p.mode {
	case ModeAuto:
		res := approval.Result{
			Decision: approval.DecisionAllow,
			Reason:   "Auto mode enabled",
		}
		p.logAudit(
			context.Background(),
			auditEventForModeDecision(req, res, "security.policy.allowed"),
		)
		return res, true, nil
	case ModeRead:
		if Category(req.Category) == CategoryRead {
			res := approval.Result{
				Decision: approval.DecisionAllow,
				Reason:   "Read operation allowed in read mode",
			}
			p.logAudit(
				context.Background(),
				auditEventForModeDecision(req, res, "security.policy.allowed"),
			)
			return res, true, nil
		}
		res := approval.Result{
			Decision: approval.DecisionDeny,
			Reason:   "Only read operations allowed in read mode",
		}
		p.logAudit(
			context.Background(),
			auditEventForModeDecision(req, res, "security.policy.denied"),
		)
		return res, true, nil
	case ModeEdit:
		if Category(req.Category) == CategoryRead {
			res := approval.Result{
				Decision: approval.DecisionAllow,
				Reason:   "Read operation allowed in edit mode",
			}
			p.logAudit(
				context.Background(),
				auditEventForModeDecision(req, res, "security.policy.allowed"),
			)
			return res, true, nil
		}
		// Write and execute operations require manual approval (not handled by this policy automatically).
		p.logAudit(context.Background(), audit.Event{
			Kind:      audit.KindPolicyDeferred,
			Category:  req.Category,
			Operation: req.Operation,
			Resource:  req.Resource,
			Decision:  "defer",
			Reason:    "manual approval required",
		})
		return approval.Result{}, false, nil
	default:
		res := approval.Result{
			Decision: approval.DecisionDeny,
			Reason:   "Unknown safety mode",
		}
		p.logAudit(
			context.Background(),
			auditEventForModeDecision(req, res, "security.policy.denied"),
		)
		return res, true, nil
	}
}

func (p *Config) logAudit(ctx context.Context, event audit.Event) {
	if p == nil || p.audit == nil {
		return
	}
	_ = p.audit.Log(ctx, event)
}

func auditEventForModeDecision(req approval.Request, res approval.Result, kind string) audit.Event {
	return audit.Event{
		Kind:      kind,
		SessionID: req.SessionID,
		Tool:      req.Tool,
		Category:  req.Category,
		Operation: req.Operation,
		Resource:  req.Resource,
		Decision:  string(res.Decision),
		Reason:    res.Reason,
		Metadata:  cloneMetadata(req.Metadata),
	}
}

func cloneMetadata(src map[string]any) map[string]any {
	if len(src) == 0 {
		return nil
	}
	dst := make(map[string]any, len(src))
	for k, v := range src {
		dst[k] = v
	}
	return dst
}
