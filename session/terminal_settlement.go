package session

type RoutingStop struct {
	Reason     string
	StopReason string
}

type StreamClosureInput struct {
	Thinking bool
}

type StreamClosureDecision struct {
	Terminal     bool
	DisplayError string
	EntryContent string
}

func DecideStreamClosure(input StreamClosureInput) StreamClosureDecision {
	if !input.Thinking {
		return StreamClosureDecision{}
	}
	const displayErr = "session event stream closed"
	return StreamClosureDecision{
		Terminal:     true,
		DisplayError: displayErr,
		EntryContent: "Error: " + displayErr,
	}
}

type ErrorSettlementInput struct {
	Err           error
	AwaitTerminal bool
}

type ErrorSettlementDecision struct {
	DisplayError  string
	EntryContent  string
	RoutingStop   *RoutingStop
	PersistSystem bool
	AwaitNext     bool
}

func DecideErrorSettlement(input ErrorSettlementInput) ErrorSettlementDecision {
	displayErr := DisplayError(input.Err)
	var routingStop *RoutingStop
	if limit, ok := ClassifyProviderLimitError(input.Err); ok {
		displayErr = limit.Display()
		routingStop = &RoutingStop{
			Reason:     limit.Reason,
			StopReason: limit.Raw,
		}
	}
	return ErrorSettlementDecision{
		DisplayError:  displayErr,
		EntryContent:  "Error: " + displayErr,
		RoutingStop:   routingStop,
		PersistSystem: input.AwaitTerminal,
		AwaitNext:     input.AwaitTerminal,
	}
}
