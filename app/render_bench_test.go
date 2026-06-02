package app

import (
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

var (
	benchmarkStringSink       string
	benchmarkIntSink          int
	benchmarkPickerItemsSink  []pickerItem
	benchmarkSessionItemsSink []sessionPickerItem
)

func BenchmarkP1StartupReadyShell(b *testing.B) {
	b.Setenv("HOME", b.TempDir())

	backend := stubBackend{sess: &stubSession{}}
	b.ReportAllocs()
	for b.Loop() {
		model := New(
			backend,
			nil,
			nil,
			"/Users/nick/github/nijaru/ion",
			"main",
			"bench",
			nil,
		)
		model.App.Ready = true
		model.App.Width = 120
		model.App.Height = 32
		model.layout()
		benchmarkStringSink = model.View().Content
	}
}

func BenchmarkP1EventToViewActiveTool(b *testing.B) {
	base := benchmarkRenderModel()
	events := benchmarkP1TurnEvents(12)

	b.ReportAllocs()
	for b.Loop() {
		model := base
		model.InFlight.Subagents = make(map[string]*SubagentProgress)
		for _, ev := range events {
			updated, _ := model.Update(ev)
			model = (*updated.(*Model))
		}
		benchmarkStringSink = model.View().Content
	}
}

func BenchmarkP1BurstAgentDeltaReduction(b *testing.B) {
	base := benchmarkRenderModel()
	deltas := benchmarkAgentDeltas(128)
	b.ReportMetric(float64(len(deltas)+1), "events/op")

	b.ReportAllocs()
	for b.Loop() {
		model := base
		model.InFlight.Subagents = make(map[string]*SubagentProgress)
		updated, _ := model.Update(session.TurnStartedEvent{})
		model = (*updated.(*Model))
		for _, ev := range deltas {
			updated, _ := model.Update(ev)
			model = (*updated.(*Model))
		}
		benchmarkIntSink = len(model.InFlight.StreamChunks)
	}
}

func BenchmarkViewReadyShell(b *testing.B) {
	model := benchmarkRenderModel()
	b.ReportAllocs()
	for b.Loop() {
		benchmarkStringSink = model.View().Content
	}
}

func BenchmarkViewStreamingAgent(b *testing.B) {
	model := benchmarkRenderModel()
	model.Progress.Mode = stateStreaming
	model.Progress.Status = "Thinking..."
	model.Progress.TurnStartedAt = time.Now().Add(-3 * time.Second)
	model.InFlight.Pending = &session.Entry{
		Role:    session.RoleAgent,
		Content: strings.Repeat("streamed assistant text with enough words to wrap cleanly ", 120),
	}

	b.ReportAllocs()
	for b.Loop() {
		benchmarkStringSink = model.View().Content
	}
}

func BenchmarkRenderReplayTranscriptEntries(b *testing.B) {
	model := benchmarkRenderModel()
	entries := benchmarkReplayEntries(240)

	b.ReportAllocs()
	for b.Loop() {
		var rendered strings.Builder
		for _, entry := range entries {
			rendered.WriteString(model.renderEntry(entry))
			rendered.WriteByte('\n')
		}
		benchmarkStringSink = rendered.String()
	}
}

func BenchmarkRankedModelPickerItems(b *testing.B) {
	items := benchmarkPickerItems(1200)

	b.ReportAllocs()
	for b.Loop() {
		benchmarkPickerItemsSink = rankedPickerItems(items, "model 11 64k")
	}
}

func BenchmarkRenderModelPickerLargeCatalog(b *testing.B) {
	model := benchmarkRenderModel()
	items := benchmarkPickerItems(1200)
	model.Picker.Overlay = &pickerOverlayState{
		title:    "Choose a model",
		items:    items,
		filtered: items,
		index:    len(items) / 2,
		purpose:  pickerPurposeModel,
	}

	b.ReportAllocs()
	for b.Loop() {
		benchmarkStringSink = model.renderPicker()
	}
}

func BenchmarkRankedSessionPickerItems(b *testing.B) {
	workdir := "/Users/nick/github/nijaru/ion"
	items := benchmarkSessionPickerItems(1200, workdir)

	b.ReportAllocs()
	for b.Loop() {
		benchmarkSessionItemsSink = rankedSessionPickerItems(items, "feature 11", workdir)
	}
}

func BenchmarkRenderSessionPickerLargeList(b *testing.B) {
	workdir := "/Users/nick/github/nijaru/ion"
	model := benchmarkRenderModel()
	items := benchmarkSessionPickerItems(1200, workdir)
	model.Picker.Session = &sessionPickerState{
		items:    items,
		filtered: items,
		index:    len(items) / 2,
	}

	b.ReportAllocs()
	for b.Loop() {
		benchmarkStringSink = model.renderSessionPicker()
	}
}

func benchmarkRenderModel() Model {
	model := New(
		stubBackend{sess: &stubSession{}},
		nil,
		nil,
		"/Users/nick/github/nijaru/ion",
		"main",
		"bench",
		nil,
	)
	model.App.Ready = true
	model.App.Width = 120
	model.App.Height = 32
	model.Input.Composer.SetWidth(max(1, model.shellWidth()-composerPromptWidth()))
	return model
}

func benchmarkPickerItems(count int) []pickerItem {
	items := make([]pickerItem, 0, count)
	for i := range count {
		label := fmt.Sprintf("model-%04d", i)
		group := "All models"
		if i%7 == 0 {
			group = "Current"
		}
		items = append(items, pickerItem{
			Label:  label,
			Value:  label,
			Group:  group,
			Detail: fmt.Sprintf("provider family %02d", i%16),
			Metrics: &pickerMetrics{
				Context: fmt.Sprintf("%dk", 32+(i%8)*32),
				Input:   fmt.Sprintf("$%.2f", float64(i%13)/10),
				Output:  fmt.Sprintf("$%.2f", float64(i%17)/10),
			},
		})
	}
	return items
}

func benchmarkSessionPickerItems(count int, workdir string) []sessionPickerItem {
	items := make([]sessionPickerItem, 0, count)
	now := time.Date(2026, 5, 21, 12, 0, 0, 0, time.UTC)
	for i := range count {
		items = append(items, sessionPickerItem{
			info: session.SessionInfo{
				ID:           fmt.Sprintf("session-%04d", i),
				CWD:          workdir,
				Model:        "qwen3.6:27b",
				Branch:       "main",
				CreatedAt:    now.Add(-time.Duration(i) * time.Hour),
				UpdatedAt:    now.Add(-time.Duration(i) * time.Minute),
				MessageCount: i%80 + 1,
				Title:        fmt.Sprintf("feature investigation %04d", i),
				Summary: fmt.Sprintf(
					"summary for turn cluster %04d with picker ranking text",
					i,
				),
				LastPreview: fmt.Sprintf("last preview message for feature %02d", i%32),
			},
		})
	}
	return items
}

func benchmarkReplayEntries(count int) []session.Entry {
	entries := make([]session.Entry, 0, count)
	for i := range count {
		entries = append(
			entries,
			session.Entry{
				Role:    session.RoleUser,
				Content: fmt.Sprintf("Please inspect the runtime transition path for case %d.", i),
			},
			session.Entry{
				Role: session.RoleAgent,
				Content: strings.Join([]string{
					fmt.Sprintf("Result for case %d:", i),
					"",
					"- runtime snapshot stayed stable",
					"- queued input preserved order",
					"- replay output remained renderable",
					"",
					"| field | value |",
					"| --- | --- |",
					"| provider | openai-compatible |",
				}, "\n"),
			},
			session.Entry{
				Role:    session.RoleTool,
				Title:   "Bash(go test ./internal/app)",
				Content: "ok github.com/nijaru/ion/internal/app 0.123s\n",
			},
		)
	}
	return entries
}

func benchmarkP1TurnEvents(deltaCount int) []session.AgentEvent {
	events := []session.AgentEvent{
		session.UserMessageEvent{Message: "inspect the workspace"},
		session.TurnStartedEvent{},
		session.StatusChangedEvent{Status: "Thinking..."},
	}
	for i := range deltaCount {
		events = append(events, session.AgentDeltaEvent{
			Delta: fmt.Sprintf("stream chunk %02d with enough text to render ", i),
		})
	}
	events = append(
		events,
		session.ToolCallStartedEvent{
			ToolUseID: "tool-1",
			ToolName:  "read",
			Args:      `{"file_path":"ai/STATUS.md"}`,
		},
		session.ToolOutputDeltaEvent{
			ToolUseID: "tool-1",
			Delta:     strings.Repeat("status line\n", 24),
		},
	)
	return events
}

func benchmarkAgentDeltas(count int) []session.AgentEvent {
	events := make([]session.AgentEvent, 0, count)
	for i := range count {
		events = append(events, session.AgentDeltaEvent{
			Delta: fmt.Sprintf("delta-%03d ", i),
		})
	}
	return events
}
