package agent

import "sync"

// ActivationLease represents a host-owned session selection staged for a
// runtime replacement. The replacement commits it only after validation and
// frontend persistence succeed; closing an uncommitted runtime rolls it back.
// This keeps a shared session store from moving the current runtime behind a
// rejected replacement.
type ActivationLease struct {
	mu       sync.Mutex
	rollback func() error
	settled  bool
}

// NewActivationLease creates an activation transaction with the supplied
// rollback operation. The callback must be independent of the runtime context
// because shutdown cancels runtime work before restoring the prior selection.
func NewActivationLease(rollback func() error) *ActivationLease {
	return &ActivationLease{rollback: rollback}
}

// Commit marks the staged activation accepted. Commit is intentionally
// infallible: the host has already applied the selection, and the only
// remaining transition is to stop considering it provisional.
func (l *ActivationLease) Commit() {
	if l == nil {
		return
	}
	l.mu.Lock()
	l.settled = true
	l.mu.Unlock()
}

// Rollback restores the previous selection once unless the activation was
// committed. It is safe for concurrent close paths and returns the host's
// restoration error without hiding it.
func (l *ActivationLease) Rollback() error {
	if l == nil {
		return nil
	}
	l.mu.Lock()
	if l.settled {
		l.mu.Unlock()
		return nil
	}
	l.settled = true
	rollback := l.rollback
	l.mu.Unlock()
	if rollback == nil {
		return nil
	}
	return rollback()
}
