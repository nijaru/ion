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

func cloneApprovalRequest(req session.ApprovalRequest) session.ApprovalRequest {
	req.Paths = append([]string(nil), req.Paths...)
	return req
}

func (m *Model) replaceApprovalRequests(requests []session.ApprovalRequest) {
	previous := m.Picker.Approval
	m.Picker.Approval = nil
	if len(requests) == 0 {
		return
	}
	queued := make([]session.ApprovalRequest, 0, len(requests)-1)
	for _, request := range requests[1:] {
		queued = append(queued, cloneApprovalRequest(request))
	}
	state := &approvalPromptState{
		request: cloneApprovalRequest(requests[0]),
		queued:  queued,
	}
	if previous != nil {
		state.resolvingID = previous.resolvingID
		if state.resolvingID == "" && previous.resolving {
			state.resolvingID = previous.request.ID
		}
		if !approvalRequestInSnapshot(requests, state.resolvingID) {
			state.resolvingID = ""
		}
	}
	state.resolving = state.resolvingID == state.request.ID
	m.Picker.Approval = state
}

func approvalRequestInSnapshot(requests []session.ApprovalRequest, id string) bool {
	if id == "" {
		return false
	}
	for _, request := range requests {
		if request.ID == id {
			return true
		}
	}
	return false
}

func approvalRequestInPrompt(prompt *approvalPromptState, id string) bool {
	if prompt == nil || id == "" {
		return false
	}
	if prompt.request.ID == id {
		return true
	}
	return approvalRequestInSnapshot(prompt.queued, id)
}

func (m *Model) addApprovalRequest(request session.ApprovalRequest) {
	request = cloneApprovalRequest(request)
	prompt := m.Picker.Approval
	if prompt == nil {
		m.Picker.Approval = &approvalPromptState{request: request}
		return
	}
	if prompt.request.ID == request.ID {
		// A request event may follow a resync snapshot. Refresh its details but
		// preserve an in-flight resolution so duplicate delivery is idempotent.
		prompt.request = request
		return
	}
	for i := range prompt.queued {
		if prompt.queued[i].ID == request.ID {
			prompt.queued[i] = request
			return
		}
	}
	prompt.queued = append(prompt.queued, request)
}

func (m *Model) resolveApprovalRequest(id string) {
	prompt := m.Picker.Approval
	if prompt == nil {
		return
	}
	if prompt.request.ID == id {
		if len(prompt.queued) == 0 {
			m.Picker.Approval = nil
			return
		}
		prompt.request = prompt.queued[0]
		prompt.queued = prompt.queued[1:]
		prompt.resolving = false
		prompt.resolvingID = ""
		return
	}
	for i := range prompt.queued {
		if prompt.queued[i].ID == id {
			prompt.queued = append(prompt.queued[:i], prompt.queued[i+1:]...)
			if prompt.resolvingID == id {
				prompt.resolvingID = ""
			}
			return
		}
	}
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
	prompt.resolvingID = requestID
	generation := m.Model.EventGeneration
	return m, func() tea.Msg {
		return approvalResolveMsg{
			generation: generation,
			requestID:  requestID,
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
	// A resolution event may have advanced the prompt before this command's
	// result arrives. Do not cancel the new request for an old command error.
	prompt := m.Picker.Approval
	if prompt == nil || prompt.resolvingID != msg.requestID ||
		!approvalRequestInPrompt(prompt, msg.requestID) {
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
