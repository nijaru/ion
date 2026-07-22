package app

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/session"
)

type approvalResolver interface {
	ResolveApproval(string, session.ApprovalDecision) error
}

func (m Model) handleApprovalKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	prompt := m.Picker.Approval
	if prompt == nil || prompt.resolving {
		return m, nil
	}
	var decision session.ApprovalDecision
	switch strings.ToLower(msg.String()) {
	case "y", "enter":
		decision = session.ApprovalAllow
	case "a":
		decision = session.ApprovalAlways
	case "n", "esc", "ctrl+c", "ctrl+d":
		decision = session.ApprovalDeny
	default:
		return m, nil
	}

	resolver, ok := m.Model.Runner.(approvalResolver)
	if !ok {
		m.Picker.Approval = nil
		return m.cancelRunningTurn("Approval control is unavailable; canceled.")
	}
	prompt.resolving = true
	requestID := prompt.request.ID
	generation := m.Model.EventGeneration
	return m, func() tea.Msg {
		return approvalResolveMsg{
			generation: generation,
			err:        resolver.ResolveApproval(requestID, decision),
		}
	}
}

func (m Model) handleApprovalResolve(msg approvalResolveMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration {
		return m, nil
	}
	if msg.err == nil {
		return m, nil
	}
	m.Picker.Approval = nil
	return m.cancelRunningTurn("Approval failed; canceled the active turn.")
}

func (m Model) renderApprovalPrompt() string {
	prompt := m.Picker.Approval
	if prompt == nil {
		return ""
	}
	req := prompt.request
	resource := strings.TrimSpace(req.Resource)
	if resource == "" {
		resource = "this operation"
	}
	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.cardTopBorder("Tool approval required"))
	b.WriteString("\n")
	b.WriteString(m.cardPaddedLine(m.st.caution.Bold(true), fmt.Sprintf("  %s: %s", req.ToolName, resource)))
	b.WriteString("\n")
	if req.Operation != "" || req.Category != "" {
		b.WriteString(m.cardPaddedLine(m.st.dim, fmt.Sprintf("  %s %s", req.Category, req.Operation)))
		b.WriteString("\n")
	}
	b.WriteString(m.cardDivider())
	b.WriteString("\n")
	if prompt.resolving {
		b.WriteString(m.cardPaddedLine(m.st.dim, "  Resolving…"))
	} else {
		b.WriteString(m.cardPaddedLine(m.st.dim, "  y allow • a always this runtime • n/Esc deny"))
	}
	b.WriteString("\n")
	b.WriteString(m.cardBottomBorder())
	return b.String()
}
