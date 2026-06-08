package session

import (
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestMessageHelpers(t *testing.T) {
	tests := []struct {
		name string
		msg  llm.Message
		role llm.Role
	}{
		{name: "user", msg: userMessage("hi"), role: llm.RoleUser},
		{name: "system", msg: systemMessage("rules"), role: llm.RoleSystem},
		{name: "assistant", msg: assistantMessage("ok"), role: llm.RoleAssistant},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.msg.Role != tt.role {
				t.Fatalf("role = %q, want %q", tt.msg.Role, tt.role)
			}
			if tt.msg.Content == "" {
				t.Fatal("content is empty")
			}
			if tt.msg.TextContent() == "" {
				t.Fatal("text content is empty")
			}
			if len(tt.msg.Parts) != 1 || tt.msg.Parts[0].Text == "" {
				t.Fatalf("parts = %+v, want one text part", tt.msg.Parts)
			}
		})
	}
}

func TestToolMessage(t *testing.T) {
	msg := toolMessage("bash", "call-1", "done")
	if msg.Role != llm.RoleTool {
		t.Fatalf("role = %q, want %q", msg.Role, llm.RoleTool)
	}
	if msg.Name != "bash" || msg.ToolID != "call-1" || msg.Content != "done" {
		t.Fatalf("unexpected tool message: %+v", msg)
	}
}

func TestAppendUser(t *testing.T) {
	sess := New("sess")
	if err := sess.AppendUser(t.Context(), "hello"); err != nil {
		t.Fatalf("AppendUser: %v", err)
	}
	messages := sess.Messages()
	if len(messages) != 1 {
		t.Fatalf("messages len = %d, want 1", len(messages))
	}
	if messages[0].Role != llm.RoleUser || messages[0].Content != "hello" {
		t.Fatalf("message = %+v, want user hello", messages[0])
	}
}
