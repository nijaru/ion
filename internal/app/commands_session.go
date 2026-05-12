package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (m Model) costBudgetNotice(inputTokens, outputTokens int, totalCost float64) string {
	totalTokens := inputTokens + outputTokens
	lines := []string{
		"Session cost",
		fmt.Sprintf("input tokens: %d", inputTokens),
		fmt.Sprintf("output tokens: %d", outputTokens),
		fmt.Sprintf("total tokens: %d", totalTokens),
		fmt.Sprintf("cost: $%.6f", totalCost),
	}
	if m.Model.Config != nil && m.Model.Config.MaxSessionCost > 0 {
		lines = append(lines, fmt.Sprintf("session limit: $%.6f", m.Model.Config.MaxSessionCost))
		remaining := m.Model.Config.MaxSessionCost - totalCost
		if remaining < 0 {
			remaining = 0
		}
		lines = append(lines, fmt.Sprintf("session remaining: $%.6f", remaining))
	}
	if m.Model.Config != nil && m.Model.Config.MaxTurnCost > 0 {
		lines = append(lines, fmt.Sprintf("turn limit: $%.6f", m.Model.Config.MaxTurnCost))
	}
	return strings.Join(lines, "\n")
}

func (m Model) handleSessionCompacted(msg sessionCompactedMsg) (Model, tea.Cmd) {
	m.Progress.Compacting = false
	m.Progress.Status = "Ready"
	m.clearProgressError()
	cmds := []tea.Cmd{m.printEntries(session.Entry{Role: session.System, Content: msg.notice})}
	if len(m.InFlight.QueuedTurns) > 0 {
		queued := m.InFlight.QueuedTurns[0]
		m.InFlight.QueuedTurns = m.InFlight.QueuedTurns[1:]
		cmds = append(cmds, func() tea.Msg { return queuedTurnMsg{text: queued} })
	}
	return m, tea.Sequence(cmds...)
}

func (m Model) handleSessionCost(msg sessionCostMsg) (Model, tea.Cmd) {
	return m, m.printEntries(session.Entry{Role: session.System, Content: msg.notice})
}

func (m Model) sessionInfoNotice() (string, error) {
	sessionID := ""
	if m.Model.Storage != nil {
		if storage.IsMaterialized(m.Model.Storage) {
			sessionID = strings.TrimSpace(m.Model.Storage.ID())
		}
	} else if m.Model.Session != nil {
		sessionID = strings.TrimSpace(m.Model.Session.ID())
	}
	if sessionID == "" {
		sessionID = "none"
	}

	provider := strings.TrimSpace(m.Model.Backend.Provider())
	model := strings.TrimSpace(m.Model.Backend.Model())
	if provider == "" {
		provider = "unknown"
	}
	if model == "" {
		model = "unknown"
	}

	inputTokens, outputTokens, totalCost := m.Progress.TokensSent, m.Progress.TokensReceived, m.Progress.TotalCost
	var entries []session.Entry
	if m.Model.Storage != nil {
		input, output, cost, err := m.Model.Storage.Usage(context.Background())
		if err != nil {
			return "", fmt.Errorf("failed to load session usage: %v", err)
		}
		inputTokens = input
		outputTokens = output
		totalCost = cost
		loaded, err := m.Model.Storage.Entries(context.Background())
		if err != nil {
			return "", fmt.Errorf("failed to load session entries: %v", err)
		}
		entries = loaded
	}

	counts := sessionEntryCounts(entries)
	lines := []string{
		"Session",
		"id: " + sessionID,
		"provider: " + provider,
		"model: " + model,
	}
	if branch := strings.TrimSpace(m.App.Branch); branch != "" {
		lines = append(lines, "branch: "+branch)
	}
	lines = append(
		lines,
		fmt.Sprintf("messages: user %d, assistant %d, tools %d, total %d",
			counts.user, counts.agent, counts.tool, counts.total),
		fmt.Sprintf("tokens: input %d, output %d, total %d",
			inputTokens, outputTokens, inputTokens+outputTokens),
		fmt.Sprintf("cost: $%.6f", totalCost),
	)
	return strings.Join(lines, "\n"), nil
}

type sessionCounts struct {
	user  int
	agent int
	tool  int
	total int
}

func sessionEntryCounts(entries []session.Entry) sessionCounts {
	var counts sessionCounts
	for _, entry := range entries {
		counts.total++
		switch entry.Role {
		case session.User:
			counts.user++
		case session.Agent:
			counts.agent++
		case session.Tool:
			counts.tool++
		}
	}
	return counts
}

func sessionTreeNotice(tree storage.SessionTree) string {
	currentID := strings.TrimSpace(tree.Current.ID)
	lines := []string{"Session tree"}
	if len(tree.Lineage) > 0 {
		lines = append(lines, "", "lineage:")
		for _, info := range tree.Lineage {
			lines = append(lines, "  "+sessionTreeLine(info, currentID))
		}
	}
	if len(tree.Children) > 0 {
		lines = append(lines, "", "children:")
		for _, info := range tree.Children {
			lines = append(lines, "  "+sessionTreeLine(info, currentID))
		}
	} else {
		lines = append(lines, "", "children: none")
	}
	return strings.Join(lines, "\n")
}

func sessionTreeLine(info storage.SessionInfo, currentID string) string {
	prefix := "- "
	if info.ID == currentID {
		prefix = "* "
	}
	label := strings.TrimSpace(info.Title)
	if label == "" {
		label = strings.TrimSpace(info.LastPreview)
	}
	if label == "" {
		label = info.ID
	}
	parts := []string{prefix + info.ID}
	if label != "" && label != info.ID {
		parts = append(parts, label)
	}
	if branch := strings.TrimSpace(info.Branch); branch != "" {
		parts = append(parts, branch)
	}
	if age := sessionAgeLabel(info.UpdatedAt); age != "" {
		parts = append(parts, age)
	}
	return strings.Join(parts, " - ")
}

func backgroundJobsNotice(jobs []session.JobInfo) string {
	if len(jobs) == 0 {
		return "Background jobs: none"
	}
	lines := []string{"Background jobs"}
	for _, job := range jobs {
		command := strings.TrimSpace(job.Command)
		if command == "" {
			command = "(unknown command)"
		}
		lines = append(lines, fmt.Sprintf(
			"- %s [%s] %s (%d bytes)",
			job.ID,
			job.Status,
			command,
			job.OutputBytes,
		))
	}
	return strings.Join(lines, "\n")
}
