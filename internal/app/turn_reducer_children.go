package app

import (
	"time"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/transcript"
)

func (r turnReducer) requestChild(name, intent string) *SubagentProgress {
	p := &SubagentProgress{
		ID:     name,
		Name:   name,
		Intent: intent,
		Status: "Requested",
	}
	if r.inFlight.Subagents == nil {
		r.inFlight.Subagents = make(map[string]*SubagentProgress)
	}
	r.inFlight.Subagents[name] = p
	r.progress.Mode = stateWorking
	return p
}

func (r turnReducer) startChild(name string) bool {
	if p, ok := r.inFlight.Subagents[name]; ok {
		p.Status = "Started"
		r.progress.Mode = stateWorking
		return true
	}
	return false
}

func (r turnReducer) appendChildDelta(name, delta string) bool {
	if p, ok := r.inFlight.Subagents[name]; ok {
		p.Output += delta
		return true
	}
	return false
}

func (r turnReducer) commitSubagentMessage(
	id, message string,
	timestamp time.Time,
) (session.Entry, bool) {
	p, ok := r.inFlight.Subagents[id]
	if !ok {
		return session.Entry{}, false
	}
	content := p.Output
	if message != "" {
		content = message
	}
	entry, _ := transcript.Subagent(p.Name, "Completed: "+content, false, timestamp)
	entry.Reasoning = p.Reasoning
	delete(r.inFlight.Subagents, id)
	r.settleChildProgress()
	return entry, true
}

func (r turnReducer) completeChild(
	name, result string,
	timestamp time.Time,
) (session.Entry, bool) {
	p, ok := r.inFlight.Subagents[name]
	if !ok {
		return session.Entry{}, false
	}
	p.Status = "Completed"
	p.Output = result
	entry, _ := transcript.Subagent(p.Name, "Completed: "+p.Output, false, timestamp)
	delete(r.inFlight.Subagents, name)
	r.settleChildProgress()
	return entry, true
}

func (r turnReducer) blockChild(name, reason string) bool {
	p, ok := r.inFlight.Subagents[name]
	if !ok {
		return false
	}
	p.Status = "Blocked"
	p.Output = "BLOCKED: " + reason
	r.progress.Mode = stateBlocked
	r.inFlight.Thinking = false
	return true
}

func (r turnReducer) failChild(
	name, err string,
	timestamp time.Time,
) (session.Entry, bool) {
	p, ok := r.inFlight.Subagents[name]
	if !ok {
		return session.Entry{}, false
	}
	p.Status = "Failed"
	p.Output = "ERROR: " + err
	entry, _ := transcript.Subagent(p.Name, "Failed: "+err, true, timestamp)
	delete(r.inFlight.Subagents, name)
	r.progress.Mode = stateError
	r.progress.LastError = "Subagent failed: " + err
	return entry, true
}

func (r turnReducer) cancelChild(
	name, reason string,
	timestamp time.Time,
) (session.Entry, bool) {
	p, ok := r.inFlight.Subagents[name]
	if !ok {
		return session.Entry{}, false
	}
	p.Status = "Canceled"
	p.Output = childCanceledContent(reason)
	entry, _ := transcript.Subagent(p.Name, p.Output, false, timestamp)
	delete(r.inFlight.Subagents, name)
	r.settleChildProgress()
	return entry, true
}

func (r turnReducer) settleChildProgress() {
	r.progress.Status = ""
	switch {
	case len(r.inFlight.Subagents) > 0:
		r.progress.Mode = stateWorking
	case r.inFlight.Thinking:
		r.progress.Mode = stateIonizing
	default:
		r.progress.Mode = stateComplete
	}
}
