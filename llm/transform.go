package llm

const missingToolResultContent = "No result provided."

// TransformRequestForCapabilities adapts a unified request to a model's
// capability constraints while preserving transcript continuity when sessions
// move across providers.
func TransformRequestForCapabilities(req *Request, caps Capabilities) {
	if req == nil {
		return
	}

	if caps.SystemRole != RoleSystem {
		rewriteSystemMessages(req, caps.SystemRole)
	}
	if !caps.Temperature {
		req.Temperature = 0
	}
	if req.ReasoningEffort != "" && !caps.SupportsReasoningControl(req.ReasoningEffort) {
		req.ReasoningEffort = ""
	}
	if req.ThinkingBudget > 0 && !caps.SupportsThinkingBudget(req.ThinkingBudget) {
		req.ThinkingBudget = 0
	}

	normalizeToolIDs(req.Messages)
	if !caps.SupportsThinking() {
		flattenUnsupportedThinking(req.Messages)
	}
	synthesizeMissingToolResults(req)
}

// PrepareRequestForCapabilities returns a provider-ready copy of req adapted
// to caps. The original request remains neutral and can be prepared again for a
// different provider or model.
func PrepareRequestForCapabilities(req *Request, caps Capabilities) (*Request, error) {
	if err := ValidateRequest(req); err != nil {
		return nil, err
	}
	prepared := req.Clone()
	TransformRequestForCapabilities(prepared, caps)
	if err := ValidateRequest(prepared); err != nil {
		return nil, err
	}
	return prepared, nil
}
