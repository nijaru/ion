package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/internal/runtime"
	"github.com/nijaru/ion/session"
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
	m.progressReducer().completeCompaction()
	cmds := []tea.Cmd{m.terminalCommit().Entries(systemEntry(msg.notice))}
	if queued := m.turnReducer().PopQueuedTurn(); queued != "" {
		cmds = append(cmds, func() tea.Msg {
			return queuedTurnMsg{text: queued, rearmSessionEvents: false}
		})
	}
	return m, tea.Sequence(cmds...)
}

func (m Model) handleSessionCost(msg sessionCostMsg) (Model, tea.Cmd) {
	return m, m.terminalCommit().Entries(systemEntry(msg.notice))
}

func loadSessionUsageCmd(generation uint64, sess session.Session) tea.Cmd {
	if sess == nil {
		return nil
	}
	return func() tea.Msg {
		usage, err := sess.Usage(context.Background())
		return sessionUsageLoadedMsg{
			generation: generation,
			input:      usage.Input,
			output:     usage.Output,
			cost:       usage.Cost.Total,
			err:        err,
		}
	}
}

func (m Model) handleSessionUsageLoaded(msg sessionUsageLoadedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || msg.err != nil {
		return m, nil
	}
	m.progressReducer().applySessionUsage(msg.input, msg.output, msg.cost)
	return m, nil
}

func (m Model) sessionCostCmd() tea.Cmd {
	return func() tea.Msg {
		inputTokens := m.Progress.TokensSent
		outputTokens := m.Progress.TokensReceived
		totalCost := m.Progress.TotalCost
		if m.Model.Storage != nil {
			usage, err := m.Model.Storage.Usage(context.Background())
			if err != nil {
				return localErrorMsg{err: fmt.Errorf("failed to load session usage: %w", err)}
			}
			inputTokens = usage.Input
			outputTokens = usage.Output
			totalCost = usage.Cost.Total
		}
		if totalCost <= 0 {
			if m.Model.Config != nil &&
				(m.Model.Config.MaxSessionCost > 0 || m.Model.Config.MaxTurnCost > 0) {
				return sessionCostMsg{
					notice: m.costBudgetNotice(inputTokens, outputTokens, totalCost),
				}
			}
			return sessionCostMsg{notice: "No API cost tracked for this session"}
		}
		return sessionCostMsg{notice: m.costBudgetNotice(inputTokens, outputTokens, totalCost)}
	}
}

func (m Model) sessionInfoCmd() tea.Cmd {
	return func() tea.Msg {
		notice, err := m.sessionInfoNotice()
		if err != nil {
			return localErrorMsg{err: err}
		}
		return localEntriesMsg{
			entries: []session.Entry{systemEntry(notice)},
		}
	}
}

func (m Model) sessionInfoNotice() (string, error) {
	sessionID := ""
	if m.Model.Runtime.Materialized {
		sessionID = m.Model.Runtime.SessionID
	}
	if m.Model.Storage != nil {
		if sessionID == "" && runtime.IsMaterialized(m.Model.Storage) {
			sessionID = strings.TrimSpace(m.Model.Storage.ID())
		}
	} else if m.Model.Session != nil {
		sessionID = strings.TrimSpace(m.Model.Session.ID())
	}
	if sessionID == "" {
		sessionID = "none"
	}

	provider := m.runtimeProvider()
	model := m.runtimeModel()
	if provider == "" {
		provider = "unknown"
	}
	if model == "" {
		model = "unknown"
	}

	inputTokens, outputTokens, totalCost := m.Progress.TokensSent, m.Progress.TokensReceived, m.Progress.TotalCost
	var entries []session.Entry
	if m.Model.Storage != nil {
		usage, err := m.Model.Storage.Usage(context.Background())
		if err != nil {
			return "", fmt.Errorf("failed to load session usage: %w", err)
		}
		inputTokens = usage.Input
		outputTokens = usage.Output
		totalCost = usage.Cost.Total
		loaded, err := m.Model.Storage.Entries(context.Background())
		if err != nil {
			return "", fmt.Errorf("failed to load session entries: %w", err)
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
		switch session.EntryRole(entry) {
		case "user":
			counts.user++
		case "assistant":
			counts.agent++
		case "tool_result":
			counts.tool++
		}
	}
	return counts
}
