package app

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

type searchStubRunner struct {
	stubRunner
	results []session.SearchResult
	err     error
}

func (r *searchStubRunner) SearchEntries(_ context.Context, _ string, _ int) ([]session.SearchResult, error) {
	return r.results, r.err
}

func TestHandleSearchCommand(t *testing.T) {
	now := time.Now()
	runner := &searchStubRunner{
		results: []session.SearchResult{
			{
				EntryID:   "entry-1",
				Role:      "user",
				Snippet:   "How do I configure <b>Postgres</b> connection pooling?",
				Timestamp: now,
			},
		},
	}

	model := readyModel(t)
	model.Model.Runner = runner

	// Test missing argument
	m, cmd := model.handleCommand("/search")
	if cmd == nil {
		t.Fatal("expected error cmd for missing arg")
	}
	_ = m

	// Test with query
	m, cmd = model.handleCommand("/search Postgres")
	if cmd == nil {
		t.Fatal("expected search async cmd")
	}

	msg := cmd()
	searchMsg, ok := msg.(sessionSearchResultsMsg)
	if !ok {
		t.Fatalf("expected sessionSearchResultsMsg, got %T", msg)
	}
	if searchMsg.query != "Postgres" {
		t.Fatalf("expected query 'Postgres', got %q", searchMsg.query)
	}

	// Dispatch message into model
	_, searchCmd := m.handleSessionSearchResults(searchMsg)
	requireTerminalCommitContains(t, searchCmd, "Search results for \"Postgres\"")
}
