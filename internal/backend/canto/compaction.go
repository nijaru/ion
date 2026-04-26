package canto

import "strings"

const ionCompactionGuidance = `Summarize this Ion coding session for reliable continuation.

Preserve:
- current user goal and immediate next step
- files, packages, task IDs, commands, and commits that matter
- decisions, constraints, approvals, denials, and unresolved blockers
- tool failures, root causes, and verification status

Discard:
- transient command noise, repeated stack traces, and already-resolved detours
- generic conversation filler

Write concise structured notes. Prefer exact paths, symbols, and IDs over prose.`

func compactionMessage(extra string) string {
	extra = strings.TrimSpace(extra)
	if extra == "" {
		return ionCompactionGuidance
	}
	return ionCompactionGuidance + "\n\nUser guidance:\n" + extra
}
