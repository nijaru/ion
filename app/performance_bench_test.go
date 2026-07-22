package app

import (
	"strings"
	"testing"

	"github.com/nijaru/ion/session"
)

func benchmarkReadyModel(b *testing.B) Model {
	b.Helper()
	model := New(nil, nil, nil, "/tmp/ion-benchmark", "main", "bench", nil)
	model = model.WithSize(120, 32)
	model.App.Ready = true
	return model
}

func BenchmarkModelViewReadyShell(b *testing.B) {
	model := benchmarkReadyModel(b)
	b.ReportAllocs()
	for b.Loop() {
		_ = model.View()
	}
}

func BenchmarkModelViewStreamingTranscript(b *testing.B) {
	model := benchmarkReadyModel(b)
	model.Progress.Mode = StateStreaming
	entry := session.Entry(&session.MessageEntry{
		Message: &session.AssistantMessage{
			Content: []session.Content{
				session.TextContent{Text: strings.Repeat("streamed assistant output ", 64)},
			},
		},
	})
	model.InFlight.Pending = &entry
	b.ReportAllocs()
	for b.Loop() {
		_ = model.View()
	}
}

func BenchmarkRenderMarkdownLongDocument(b *testing.B) {
	model := benchmarkReadyModel(b)
	document := strings.Repeat("# Heading\n\nA paragraph with **bold** and `code`.\n\n", 32) +
		"```go\nfunc main() { fmt.Println(\"hello\") }\n```\n"
	b.ReportAllocs()
	for b.Loop() {
		_ = model.renderMarkdownContent(document)
	}
}
