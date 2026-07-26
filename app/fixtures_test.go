package app

// fixtures_test.go — app-local canonical Entry fixtures replacing session.TestEntry.
// Sol 0A: "Introduce app-local canonical fixture builders producing real MessageEntry/UserMessage/AssistantMessage/ToolResultMessage"

import (
	"time"

	"github.com/nijaru/ion/session"
)

// testUserEntry builds a MessageEntry wrapping a UserMessage (RoleUser).
func testUserEntry(content string) session.Entry {
	return &session.MessageEntry{
		Message: &session.UserMessage{
			Content: []session.Content{session.TextContent{Text: content}},
		},
	}
}

// testAgentEntry builds an assistant entry with optional reasoning (RoleAgent).
// Content -> TextContent, Reasoning -> ThinkingContent.
// Empty both => empty AssistantMessage (used by turn_reducer tests).
func testAgentEntry(content string, reasoning string) session.Entry {
	var blocks []session.Content
	if reasoning != "" {
		blocks = append(blocks, session.ThinkingContent{Text: reasoning})
	}
	if content != "" {
		blocks = append(blocks, session.TextContent{Text: content})
	}
	// nil Content is valid — represents empty assistant entry
	return &session.MessageEntry{
		Message: &session.AssistantMessage{Content: blocks},
	}
}

func testUserEntryWithTS(content string, ts time.Time) session.Entry {
	e := &session.MessageEntry{
		EntryBase: session.EntryBase{Timestamp: ts},
		Message: &session.UserMessage{
			Content:   []session.Content{session.TextContent{Text: content}},
			Timestamp: ts,
		},
	}
	return e
}

// testToolEntry builds a tool-result entry (RoleTool). Title used for label formatting.
func testToolEntry(title, content string, isError bool) session.Entry {
	var blocks []session.Content
	if content != "" {
		blocks = []session.Content{session.TextContent{Text: content}}
	}
	return &session.MessageEntry{
		Message: &session.ToolResultMessage{
			ToolName: title,
			Title:    title,
			Content:  blocks,
			IsError:  isError,
		},
	}
}
