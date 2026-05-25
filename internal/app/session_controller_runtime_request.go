package app

type runtimeRequestController struct {
	model *Model
}

func (m *Model) runtimeRequest() runtimeRequestController {
	return runtimeRequestController{model: m}
}

func (c runtimeRequestController) begin(status string) uint64 {
	c.model.Model.RuntimeSwitchRequest++
	requestID := c.model.Model.RuntimeSwitchRequest
	c.model.progressReducer().beginLocalStatus(status)
	return requestID
}

func (c runtimeRequestController) matches(requestID uint64) bool {
	return requestID == 0 || requestID == c.model.Model.RuntimeSwitchRequest
}

func (c runtimeRequestController) finish(requestID uint64) bool {
	if !c.matches(requestID) {
		return false
	}
	c.model.Model.RuntimeSwitchRequest = 0
	c.model.progressReducer().clearLocalBusyStatus()
	return true
}

func (c runtimeRequestController) clear() {
	c.model.Model.RuntimeSwitchRequest = 0
	c.model.progressReducer().clearLocalBusyStatus()
}
