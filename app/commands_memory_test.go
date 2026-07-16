package app

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

type stubMemoryController struct {
	records        []MemoryRecord
	queries        []string
	includeDeleted []bool
	deleted        []string
	restored       []string
	err            error
}

func (s *stubMemoryController) Search(_ context.Context, query string, includeDeleted bool, _ int) ([]MemoryRecord, error) {
	s.queries = append(s.queries, query)
	s.includeDeleted = append(s.includeDeleted, includeDeleted)
	return append([]MemoryRecord(nil), s.records...), s.err
}

func (s *stubMemoryController) Delete(_ context.Context, id string) error {
	s.deleted = append(s.deleted, id)
	return s.err
}

func (s *stubMemoryController) Restore(_ context.Context, id string) error {
	s.restored = append(s.restored, id)
	return s.err
}

func TestMemoryCommandUsesExplicitOperations(t *testing.T) {
	controller := &stubMemoryController{
		records: []MemoryRecord{{
			ID:        "mem_1",
			Content:   "keep this note",
			Tags:      "design",
			CreatedAt: time.Date(2026, 7, 16, 1, 2, 3, 0, time.UTC),
		}},
	}
	model := readyModel(t).WithMemory(controller)

	_, cmd := model.handleCommand("/memory search session tree")
	if cmd == nil || len(controller.queries) != 1 || controller.queries[0] != "session tree" {
		t.Fatalf("search command = cmd %v, queries %#v", cmd != nil, controller.queries)
	}
	if controller.includeDeleted[0] {
		t.Fatal("search unexpectedly included deleted memory")
	}

	_, cmd = model.handleCommand("/memory forget mem_1")
	if cmd == nil || len(controller.deleted) != 1 || controller.deleted[0] != "mem_1" {
		t.Fatalf("forget command = cmd %v, deleted %#v", cmd != nil, controller.deleted)
	}
	_, cmd = model.handleCommand("/memory restore mem_1")
	if cmd == nil || len(controller.restored) != 1 || controller.restored[0] != "mem_1" {
		t.Fatalf("restore command = cmd %v, restored %#v", cmd != nil, controller.restored)
	}
	_, cmd = model.handleCommand("/memory all")
	if cmd == nil || len(controller.queries) != 2 || !controller.includeDeleted[1] {
		t.Fatalf("all command = cmd %v, queries %#v, includeDeleted %#v", cmd != nil, controller.queries, controller.includeDeleted)
	}
}

func TestMemoryCommandReportsUnavailableAndUsageErrors(t *testing.T) {
	model := readyModel(t)
	_, cmd := model.handleCommand("/memory")
	if cmd == nil {
		t.Fatal("unconfigured memory command returned no error")
	}
	if err := localErrorFromMsg(t, cmd()); err == nil || err.Error() != "workspace memory is unavailable" {
		t.Fatalf("unconfigured memory error = %v", err)
	}

	controller := &stubMemoryController{}
	model = model.WithMemory(controller)
	_, cmd = model.handleCommand("/memory forget")
	if cmd == nil {
		t.Fatal("invalid memory usage returned no error")
	}
	if err := localErrorFromMsg(t, cmd()); err == nil || err.Error() != "usage: /memory [search <query>|forget <id>|restore <id>|all]" {
		t.Fatalf("usage error = %v", err)
	}

	controller.err = errors.New("memory unavailable")
	_, cmd = model.handleCommand("/memory search note")
	if cmd == nil {
		t.Fatal("memory backend error returned no command")
	}
	if err := localErrorFromMsg(t, cmd()); err == nil || err.Error() != "memory unavailable" {
		t.Fatalf("backend error = %v", err)
	}
}

func TestFormatMemoryRecords(t *testing.T) {
	got := formatMemoryRecords([]MemoryRecord{{
		ID:        "mem_1",
		Content:   "first line\nsecond line",
		Tags:      "tag",
		CreatedAt: time.Date(2026, 7, 16, 1, 2, 3, 0, time.UTC),
		Deleted:   true,
	}}, true)
	want := "mem_1  deleted  [tag]  2026-07-16 01:02:03Z\n  first line\n  second line"
	if got != want {
		t.Fatalf("formatted memory = %q, want %q", got, want)
	}
}

func TestFormatMemoryRecordsIsBounded(t *testing.T) {
	got := formatMemoryRecords([]MemoryRecord{{ID: "mem_1", Content: strings.Repeat("x", maxMemoryDisplayBytes)}}, false)
	if len(got) > maxMemoryDisplayBytes+100 {
		t.Fatalf("formatted memory length = %d, want bounded output", len(got))
	}
	if !strings.Contains(got, "memory output truncated") {
		t.Fatalf("bounded memory output has no truncation notice: %q", got[len(got)-100:])
	}
}
