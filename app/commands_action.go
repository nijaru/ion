package app

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

type actionReconciledMsg struct {
	generation uint64
	requestID  uint64
	action     session.ActionRecord
	err        error
}

func (m Model) handleActionsCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) == 1 {
		if len(m.Model.Recovery) == 0 {
			return m, m.terminalCommit().Entries(systemEntry("No unsettled external actions."))
		}
		return m, m.terminalCommit().Entries(systemEntry(actionRecoverySummary(m.Model.Recovery)))
	}
	if len(fields) < 4 || fields[1] != "reconcile" {
		return m, cmdError("usage: /actions [reconcile <action-id> <completed|failed> <evidence>]")
	}
	if m.Model.RecoveryRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("reconciling an action"))
	}
	actionID := strings.TrimSpace(fields[2])
	if actionID == "" {
		return m, cmdError("action ID is required")
	}
	var known bool
	for _, action := range m.Model.Recovery {
		if action.ID == actionID {
			known = true
			if action.State != session.ActionIndeterminate {
				return m, cmdError(
					fmt.Sprintf(
						"action %q is %s; only indeterminate actions can be reconciled",
						actionID,
						action.State,
					),
				)
			}
			break
		}
	}
	if !known {
		return m, cmdError(fmt.Sprintf("action %q is not an unsettled action; run /actions", actionID))
	}
	var state session.ActionState
	switch strings.ToLower(strings.TrimSpace(fields[3])) {
	case string(session.ActionCompleted):
		state = session.ActionCompleted
	case string(session.ActionFailed):
		state = session.ActionFailed
	default:
		return m, cmdError("reconciliation outcome must be completed or failed")
	}
	evidence := strings.TrimSpace(strings.Join(fields[4:], " "))
	if evidence == "" {
		return m, cmdError("reconciliation evidence is required")
	}
	resolver, ok := m.Model.Runner.(agent.ActionRecovery)
	if !ok {
		return m, cmdError("active runtime does not support action reconciliation")
	}
	m.Model.RecoveryRequest++
	requestID := m.Model.RecoveryRequest
	generation := m.Model.EventGeneration
	ctx := m.runtimeOperationContext()
	return m, func() tea.Msg {
		action, err := resolver.ReconcileAction(
			ctx, actionID, state, evidence, "", "", "",
		)
		return actionReconciledMsg{
			generation: generation,
			requestID:  requestID,
			action:     action,
			err:        err,
		}
	}
}

func (m Model) handleActionReconciled(msg actionReconciledMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || msg.requestID != m.Model.RecoveryRequest {
		return m, nil
	}
	m.Model.RecoveryRequest = 0
	if msg.err != nil {
		return m, cmdError(fmt.Sprintf("reconcile action: %v", msg.err))
	}
	remaining := m.Model.Recovery[:0]
	for _, action := range m.Model.Recovery {
		if action.ID != msg.action.ID {
			remaining = append(remaining, action)
		}
	}
	m.Model.Recovery = remaining
	notice := fmt.Sprintf(
		"Reconciled action %s as %s. Review the evidence before retrying.",
		msg.action.ID,
		msg.action.State,
	)
	return m, m.terminalCommit().Entries(systemEntry(notice))
}
