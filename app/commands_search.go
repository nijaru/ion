package app

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/agent"
	"github.com/nijaru/ion/session"
)

func (m Model) handleSearchCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) < 2 {
		return m, cmdError("usage: /search <query>")
	}
	query := strings.Join(fields[1:], " ")

	searcher, ok := m.Model.Runner.(agent.SessionSearcher)
	if !ok {
		return m, cmdError("search is not supported by the active runtime")
	}

	ctx := m.runtimeOperationContext()
	generation := m.Model.EventGeneration

	return m, func() tea.Msg {
		results, err := searcher.SearchEntries(ctx, query, 10)
		return sessionSearchResultsMsg{
			generation: generation,
			query:      query,
			results:    results,
			err:        err,
		}
	}
}

type sessionSearchResultsMsg struct {
	generation uint64
	query      string
	results    []session.SearchResult
	err        error
}

func (m Model) handleSessionSearchResults(msg sessionSearchResultsMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration {
		return m, nil
	}
	if msg.err != nil {
		return m.handleLocalError(fmt.Errorf("search failed: %w", msg.err))
	}
	if len(msg.results) == 0 {
		return m, m.terminalCommit().Entries(systemEntry(fmt.Sprintf("No matches found for %q", msg.query)))
	}

	var lines []string
	suffix := "es"
	if len(msg.results) == 1 {
		suffix = ""
	}
	lines = append(lines, fmt.Sprintf("Search results for %q (%d match%s):", msg.query, len(msg.results), suffix))
	for i, res := range msg.results {
		lines = append(lines, fmt.Sprintf("  %d. [%s] %s (%s)", i+1, res.Role, res.Snippet, res.Timestamp.Format("15:04:05")))
	}
	return m, m.terminalCommit().Entries(systemEntry(strings.Join(lines, "\n")))
}
