package session

import "testing"

func TestMessageTextIncludesToolResultText(t *testing.T) {
	message := &ToolResultMessage{
		Content: []Content{
			TextContent{Text: "tool output"},
			ImageContent{Data: []byte("image"), MimeType: "image/png"},
		},
	}
	if got := MessageText(message); got != "tool output" {
		t.Fatalf("MessageText() = %q, want tool output", got)
	}
}
