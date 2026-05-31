package session

type RuntimeRequestBeginInput struct {
	Current uint64
	Status  string
}

type RuntimeRequestBeginDecision struct {
	RequestID      uint64
	Status         string
	SetLocalStatus bool
}

func BeginRuntimeRequest(input RuntimeRequestBeginInput) RuntimeRequestBeginDecision {
	return RuntimeRequestBeginDecision{
		RequestID:      input.Current + 1,
		Status:         input.Status,
		SetLocalStatus: true,
	}
}

func RuntimeRequestMatches(active, requestID uint64) bool {
	return requestID == 0 || requestID == active
}

type RuntimeRequestFinishDecision struct {
	Matched          bool
	Active           uint64
	ClearLocalStatus bool
}

func FinishRuntimeRequest(active, requestID uint64) RuntimeRequestFinishDecision {
	if !RuntimeRequestMatches(active, requestID) {
		return RuntimeRequestFinishDecision{Active: active}
	}
	return RuntimeRequestFinishDecision{
		Matched:          true,
		ClearLocalStatus: true,
	}
}

type RuntimeRequestClearDecision struct {
	Active           uint64
	ClearLocalStatus bool
}

func ClearRuntimeRequest() RuntimeRequestClearDecision {
	return RuntimeRequestClearDecision{ClearLocalStatus: true}
}
