package app

import (
	"context"
	"fmt"
	"path/filepath"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
)

func (m Model) rewindCheckpointCommand(id string, confirmed bool) (Model, tea.Cmd) {
	if m.Model.Checkpoints == nil {
		return m, cmdError("checkpoint store is unavailable")
	}
	cp, err := m.Model.Checkpoints.Load(id)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load checkpoint: %v", err))
	}
	if !sameWorkspace(cp.Workspace, m.App.Workdir) {
		return m, cmdError("checkpoint belongs to a different workspace")
	}
	plan, err := m.Model.Checkpoints.AnalyzeRestore(context.Background(), cp)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to analyze checkpoint: %v", err))
	}
	if len(plan.Conflicts) == 0 {
		return m, m.printEntries(session.Entry{
			Role: session.System,
			Content: fmt.Sprintf(
				"Checkpoint %s already matches this workspace; nothing to rewind.",
				cp.ID,
			),
		})
	}
	if !confirmed {
		return m, m.printEntries(session.Entry{
			Role:    session.System,
			Content: rewindPreview(cp.ID, plan),
		})
	}

	before := session.Entry{
		Role: session.System,
		Content: fmt.Sprintf(
			"Rewind starting: checkpoint %s will restore %d path(s).",
			cp.ID,
			len(plan.Conflicts),
		),
	}
	report, err := m.Model.Checkpoints.Restore(
		context.Background(),
		cp,
		ionworkspace.RestoreOptions{AllowConflicts: true},
	)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to restore checkpoint: %v", err))
	}
	after := session.Entry{Role: session.System, Content: rewindReport(cp.ID, report)}
	return m, m.printEntries(before, after)
}

func sameWorkspace(a, b string) bool {
	aAbs, err := filepath.Abs(a)
	if err != nil {
		return false
	}
	bAbs, err := filepath.Abs(b)
	if err != nil {
		return false
	}
	return filepath.Clean(aAbs) == filepath.Clean(bAbs)
}

func rewindPreview(id string, plan ionworkspace.RestorePlan) string {
	lines := []string{
		"Rewind preview: " + id,
		fmt.Sprintf("%d path(s) would change.", len(plan.Conflicts)),
		"Run /rewind " + id + " --confirm to restore this checkpoint.",
		"",
	}
	for i, conflict := range plan.Conflicts {
		if i == 12 {
			lines = append(lines, fmt.Sprintf("... and %d more", len(plan.Conflicts)-i))
			break
		}
		lines = append(lines, fmt.Sprintf(
			"- %s %s (current: %s, checkpoint: %s)",
			conflict.Action,
			conflict.Path,
			conflict.Current,
			conflict.Target,
		))
	}
	return strings.Join(lines, "\n")
}

func rewindReport(id string, report ionworkspace.RestoreReport) string {
	lines := []string{
		"Rewind complete: " + id,
		fmt.Sprintf("restored: %d", len(report.Restored)),
		fmt.Sprintf("removed: %d", len(report.Removed)),
	}
	return strings.Join(lines, "\n")
}
