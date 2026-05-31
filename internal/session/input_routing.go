package session

type BusyInputRoute string

const (
	BusyInputRouteSteer      BusyInputRoute = "steer"
	BusyInputRouteFollowUp   BusyInputRoute = "follow_up"
	BusyInputRouteLocalQueue BusyInputRoute = "local_queue"
)

type BusyInputRouting struct {
	Mode             string
	Thinking         bool
	Compacting       bool
	SupportsSteering bool
	SupportsFollowUp bool
}

func RouteBusyInput(input BusyInputRouting) BusyInputRoute {
	if !input.Thinking || input.Compacting {
		return BusyInputRouteLocalQueue
	}
	if input.Mode == "steer" && input.SupportsSteering {
		return BusyInputRouteSteer
	}
	if input.SupportsFollowUp {
		return BusyInputRouteFollowUp
	}
	return BusyInputRouteLocalQueue
}
