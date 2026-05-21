package app

import "time"

type progressReducer struct {
	progress *ProgressState
}

func (m *Model) progressReducer() progressReducer {
	return progressReducer{progress: &m.Progress}
}

func (r progressReducer) beginLocalStatus(status string) {
	r.setLocalStatus(status)
}

func (r progressReducer) clearLocalBusyStatus() {
	if r.progress.LocalStatus != "" {
		r.setLocalStatus("")
	}
	if isLocalBusyStatus(r.progress.Status) {
		r.setStatus("")
	}
}

func (r progressReducer) beginCompaction() {
	r.progress.Compacting = true
	r.setStatus("Compacting context...")
}

func (r progressReducer) completeCompaction() {
	r.progress.Compacting = false
	r.progress.ContextTokens = 0
	r.setStatus("Ready")
	r.clearError()
}

func (r progressReducer) clearError() {
	if r.progress.Mode == stateError {
		r.progress.Mode = stateReady
	}
	r.progress.LastError = ""
}

func (r progressReducer) setReasoningEffort(value string) {
	r.progress.ReasoningEffort = value
}

func (r progressReducer) applyRuntimeSnapshot(snapshot runtimeSnapshot) {
	r.setReasoningEffort(snapshot.Reasoning)
	if snapshot.Status != "" {
		r.setStatus(snapshot.Status)
	}
}

func (r progressReducer) markRuntimeReady() {
	r.progress.Mode = stateReady
}

func (r progressReducer) resetSessionUsage() {
	r.progress.TokensSent = 0
	r.progress.TokensReceived = 0
	r.progress.ContextTokens = 0
	r.progress.TotalCost = 0
}

func (r progressReducer) applySessionUsage(input, output int, cost float64) {
	r.progress.TokensSent = input
	r.progress.TokensReceived = output
	r.progress.TotalCost = cost
}

func (r progressReducer) setStatus(status string) {
	r.progress.Status = status
	if status == "" {
		r.progress.StatusUpdatedAt = time.Time{}
		return
	}
	r.progress.StatusUpdatedAt = time.Now()
}

func (r progressReducer) setLocalStatus(status string) {
	r.progress.LocalStatus = status
	if status == "" {
		r.progress.LocalStatusAt = time.Time{}
		return
	}
	r.progress.LocalStatusAt = time.Now()
}
