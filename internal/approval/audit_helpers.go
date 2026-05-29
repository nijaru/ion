package approval

import (
	"context"
)

func (m *Gate) logAudit(ctx context.Context, event AuditEvent) {
	if m == nil {
		return
	}
	m.mu.Lock()
	logger := m.audit
	m.mu.Unlock()
	if logger == nil {
		return
	}
	_ = logger.Log(ctx, event)
}

func auditEventForApprovalResolution(
	sessionID, toolName string,
	requirement Requirement,
	result Result,
) AuditEvent {
	kind := AuditKindToolAllowed
	if result.Decision == DecisionDeny {
		kind = AuditKindToolDenied
	}
	return AuditEvent{
		Kind:      kind,
		SessionID: sessionID,
		Tool:      toolName,
		Category:  requirement.Category,
		Operation: requirement.Operation,
		Resource:  requirement.Resource,
		Decision:  string(result.Decision),
		Reason:    result.Reason,
		Metadata:  cloneMetadata(requirement.Metadata),
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
