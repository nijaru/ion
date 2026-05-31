package app

import "github.com/nijaru/ion/internal/session"

type runtimeRequestController struct {
	model *Model
}

func (m *Model) runtimeRequest() runtimeRequestController {
	return runtimeRequestController{model: m}
}

func (c runtimeRequestController) begin(status string) uint64 {
	decision := session.BeginRuntimeRequest(session.RuntimeRequestBeginInput{
		Current: c.model.Model.RuntimeSwitchRequest,
		Status:  status,
	})
	c.model.Model.RuntimeSwitchRequest = decision.RequestID
	if decision.SetLocalStatus {
		c.model.progressReducer().beginLocalStatus(decision.Status)
	}
	return decision.RequestID
}

func (c runtimeRequestController) matches(requestID uint64) bool {
	return session.RuntimeRequestMatches(c.model.Model.RuntimeSwitchRequest, requestID)
}

func (c runtimeRequestController) finish(requestID uint64) bool {
	decision := session.FinishRuntimeRequest(c.model.Model.RuntimeSwitchRequest, requestID)
	if !decision.Matched {
		return false
	}
	c.model.Model.RuntimeSwitchRequest = decision.Active
	if decision.ClearLocalStatus {
		c.model.progressReducer().clearLocalBusyStatus()
	}
	return true
}

func (c runtimeRequestController) clear() {
	decision := session.ClearRuntimeRequest()
	c.model.Model.RuntimeSwitchRequest = decision.Active
	if decision.ClearLocalStatus {
		c.model.progressReducer().clearLocalBusyStatus()
	}
}
