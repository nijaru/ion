package app

import (
	"context"
	"fmt"
	"strings"
	"unicode"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
)

const checkpointListLimit = 20

func (m Model) handleRewindCommand(fields []string) (Model, tea.Cmd) {
	if m.Model.Checkpoints == nil {
		return m, cmdError("workspace checkpoints are unavailable")
	}
	requestID := (&m).beginCheckpointRequest()
	switch {
	case len(fields) == 1:
		return m, checkpointListCmd(m.Model.Checkpoints, requestID)
	case len(fields) == 2:
		return m, checkpointPlanCmd(m.Model.Checkpoints, requestID, fields[1])
	case len(fields) == 3 && fields[2] == "--apply":
		return m, checkpointRestoreCmd(m.Model.Checkpoints, requestID, fields[1])
	default:
		return m, cmdError("usage: /rewind [checkpoint-id [--apply]]")
	}
}

func (m *Model) beginCheckpointRequest() uint64 {
	m.Model.CheckpointRequest++
	return m.Model.CheckpointRequest
}

func checkpointListCmd(controller CheckpointController, requestID uint64) tea.Cmd {
	return func() tea.Msg {
		items, err := controller.List(context.Background(), checkpointListLimit)
		return checkpointListMsg{requestID: requestID, items: items, err: err}
	}
}

func checkpointPlanCmd(controller CheckpointController, requestID uint64, id string) tea.Cmd {
	return func() tea.Msg {
		plan, err := controller.Plan(context.Background(), id)
		return checkpointPlanMsg{requestID: requestID, plan: plan, err: err}
	}
}

func checkpointRestoreCmd(controller CheckpointController, requestID uint64, id string) tea.Cmd {
	return func() tea.Msg {
		report, err := controller.Restore(context.Background(), id)
		return checkpointRestoredMsg{requestID: requestID, id: id, report: report, err: err}
	}
}

func (m Model) handleCheckpointList(msg checkpointListMsg) (Model, tea.Cmd) {
	if msg.requestID != m.Model.CheckpointRequest {
		return m, nil
	}
	if msg.err != nil {
		return m, cmdError(msg.err.Error())
	}
	return m, m.terminalCommit().Help(formatCheckpointList(msg.items))
}

func (m Model) handleCheckpointPlan(msg checkpointPlanMsg) (Model, tea.Cmd) {
	if msg.requestID != m.Model.CheckpointRequest {
		return m, nil
	}
	if msg.err != nil {
		return m, cmdError(msg.err.Error())
	}
	return m, m.terminalCommit().Help(formatCheckpointPlan(msg.plan))
}

func (m Model) handleCheckpointRestored(msg checkpointRestoredMsg) (Model, tea.Cmd) {
	if msg.requestID != m.Model.CheckpointRequest {
		return m, nil
	}
	if msg.err != nil {
		return m, cmdError(msg.err.Error())
	}
	return m, m.terminalCommit().Help(formatCheckpointReport(msg.id, msg.report))
}

func formatCheckpointList(items []CheckpointInfo) string {
	if len(items) == 0 {
		return "No workspace checkpoints. File mutations create them automatically."
	}
	var output strings.Builder
	output.WriteString("Workspace checkpoints\n\n")
	for _, item := range items {
		fmt.Fprintf(
			&output,
			"%s  %s  (%d path%s)\n",
			sanitizeCheckpointText(item.ID),
			item.CreatedAt.UTC().Format("2006-01-02 15:04:05Z"),
			item.PathCount,
			pluralSuffix(item.PathCount),
		)
	}
	output.WriteString("\n/rewind <checkpoint-id> previews changes; add --apply to restore.")
	return limitCheckpointOutput(output.String())
}

func formatCheckpointPlan(plan CheckpointPlan) string {
	if len(plan.Conflicts) == 0 {
		return fmt.Sprintf(
			"Checkpoint %s is already current; no files would change.",
			sanitizeCheckpointText(plan.ID),
		)
	}
	var output strings.Builder
	fmt.Fprintf(
		&output,
		"Checkpoint %s would change %d path%s:\n",
		sanitizeCheckpointText(plan.ID),
		len(plan.Conflicts),
		pluralSuffix(len(plan.Conflicts)),
	)
	for _, conflict := range plan.Conflicts {
		fmt.Fprintf(
			&output,
			"  %s  %s\n",
			sanitizeCheckpointText(conflict.Action),
			sanitizeCheckpointText(conflict.Path),
		)
	}
	output.WriteString("\nRun /rewind ")
	output.WriteString(sanitizeCheckpointText(plan.ID))
	output.WriteString(" --apply to restore after reviewing the paths.")
	return limitCheckpointOutput(output.String())
}

func formatCheckpointReport(id string, report CheckpointReport) string {
	return fmt.Sprintf(
		"Restored checkpoint %s: %d path%s restored, %d path%s removed.",
		sanitizeCheckpointText(id),
		len(report.Restored),
		pluralSuffix(len(report.Restored)),
		len(report.Removed),
		pluralSuffix(len(report.Removed)),
	)
}

func pluralSuffix(count int) string {
	if count == 1 {
		return ""
	}
	return "s"
}

func sanitizeCheckpointText(value string) string {
	var output strings.Builder
	for _, r := range value {
		if unicode.IsControl(r) || unicode.Is(unicode.Cf, r) {
			if r <= 0xffff {
				fmt.Fprintf(&output, "\\u%04x", r)
			} else {
				fmt.Fprintf(&output, "\\U%08x", r)
			}
			continue
		}
		output.WriteRune(r)
	}
	return output.String()
}

func limitCheckpointOutput(value string) string {
	const maxBytes = 50 * 1024
	if len(value) <= maxBytes {
		return value
	}
	cut := maxBytes
	for cut > 0 && !utf8.ValidString(value[:cut]) {
		cut--
	}
	return value[:cut] + "\n\n[checkpoint output truncated]"
}
