package session

import "slices"

// ExportRunTree converts a session's event log into a structured RunLog and,
// when load is provided, recursively attaches child runs referenced by durable
// child lifecycle events.
func ExportRunTree(sess *Session, load func(sessionID string) (*Session, error)) (*RunLog, error) {
	return exportRunTree(sess, load, make(map[string]struct{}))
}

func exportRunTree(
	sess *Session,
	load func(sessionID string) (*Session, error),
	seen map[string]struct{},
) (*RunLog, error) {
	traj, err := exportRun(sess)
	if err != nil {
		return nil, err
	}
	if load == nil {
		return traj, nil
	}
	if _, ok := seen[sess.ID()]; ok {
		return traj, nil
	}
	seen[sess.ID()] = struct{}{}
	defer delete(seen, sess.ID())

	childByID := make(map[string]*ChildRunLog)
	for e := range sess.All() {
		switch e.Type {
		case ChildRequested:
			data, ok, err := e.ChildRequestedData()
			if err != nil {
				return nil, err
			}
			if !ok {
				continue
			}
			childByID[data.ChildID] = &ChildRunLog{
				ChildID:   data.ChildID,
				SessionID: data.ChildSessionID,
				AgentID:   data.AgentID,
				Mode:      data.Mode,
				Status:    ChildStatusRequested,
				Metadata:  data.Metadata,
			}
		case ChildStarted:
			data, ok, err := e.ChildStartedData()
			if err != nil {
				return nil, err
			}
			if !ok {
				continue
			}
			child := ensureChildRun(childByID, data.ChildID, data.ChildSessionID)
			child.AgentID = data.AgentID
			child.Status = ChildStatusRunning
		case ChildBlocked:
			data, ok, err := e.ChildBlockedData()
			if err != nil {
				return nil, err
			}
			if !ok {
				continue
			}
			child := ensureChildRun(childByID, data.ChildID, data.ChildSessionID)
			child.Status = ChildStatusBlocked
		case ChildCompleted:
			data, ok, err := e.ChildCompletedData()
			if err != nil {
				return nil, err
			}
			if !ok {
				continue
			}
			child := ensureChildRun(childByID, data.ChildID, data.ChildSessionID)
			child.Status = ChildStatusCompleted
			child.Summary = data.Summary
		case ChildFailed:
			data, ok, err := e.ChildFailedData()
			if err != nil {
				return nil, err
			}
			if !ok {
				continue
			}
			child := ensureChildRun(childByID, data.ChildID, data.ChildSessionID)
			child.Status = ChildStatusFailed
		case ChildCanceled:
			data, ok, err := e.ChildCanceledData()
			if err != nil {
				return nil, err
			}
			if !ok {
				continue
			}
			child := ensureChildRun(childByID, data.ChildID, data.ChildSessionID)
			child.Status = ChildStatusCanceled
		case ChildMerged:
			data, ok, err := e.ChildMergedData()
			if err != nil {
				return nil, err
			}
			if !ok {
				continue
			}
			child := ensureChildRun(childByID, data.ChildID, data.ChildSessionID)
			child.Status = ChildStatusMerged
		case ArtifactRecorded:
			data, ok, err := e.ArtifactRecordedData()
			if err != nil {
				return nil, err
			}
			if !ok || data.ChildID == "" || IsWorkspaceFileReferenceArtifact(data.Artifact) {
				continue
			}
			child := ensureChildRun(childByID, data.ChildID, data.SessionID)
			child.Artifacts = append(child.Artifacts, data.Artifact)
		}
	}

	childIDs := make([]string, 0, len(childByID))
	for childID := range childByID {
		childIDs = append(childIDs, childID)
	}
	slices.Sort(childIDs)

	traj.ChildRuns = make([]ChildRunLog, 0, len(childIDs))
	for _, childID := range childIDs {
		child := childByID[childID]
		if child.SessionID != "" {
			childSess, err := load(child.SessionID)
			if err != nil {
				return nil, err
			}
			if childSess != nil {
				child.Run, err = exportRunTree(childSess, load, seen)
				if err != nil {
					return nil, err
				}
			}
		}
		traj.ChildRuns = append(traj.ChildRuns, *child)
	}
	return traj, nil
}

func ensureChildRun(childByID map[string]*ChildRunLog, childID, sessionID string) *ChildRunLog {
	if child, ok := childByID[childID]; ok {
		if child.SessionID == "" {
			child.SessionID = sessionID
		}
		return child
	}
	child := &ChildRunLog{
		ChildID:   childID,
		SessionID: sessionID,
	}
	childByID[childID] = child
	return child
}
