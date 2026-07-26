// Fuzz tests for agent internals.
package agent

import (
	"context"
	"encoding/json"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// FuzzStreamAccumulator tests the accumulator against arbitrary chunk sequences.
func FuzzStreamAccumulator(f *testing.F) {
	f.Add("hello", "toolUse", `call-1`, "test_tool", `{}`)
	f.Fuzz(func(t *testing.T, content string, stopReason string, callID string, callName string, callArgs string) {
		acc := &llm.StreamAccumulator{}
		chunk := &llm.Chunk{Content: content, StopReason: llm.StopReason(stopReason)}
		if callID != "" {
			chunk.Calls = []llm.Call{{
				ID: callID,
				Function: struct {
					Name      string `json:"name"`
					Arguments string `json:"arguments"`
				}{Name: callName, Arguments: callArgs},
			}}
		}
		acc.Add(chunk)
		resp := acc.Response()
		_ = resp.StopReason
		_ = resp.Blocks
		_ = resp.Usage
	})
}

// FuzzRunLoop verifies the loop doesn't panic on malformed configs.
func FuzzRunLoop(f *testing.F) {
	f.Add("test_model", "false", "hello", "stop")
	f.Fuzz(func(t *testing.T, modelID string, toolResultIsError string, toolContent string, stopReason string) {
		defer func() {
			if r := recover(); r != nil {
				t.Fatalf("RunLoop panicked: %v", r)
			}
		}()

		isError := toolResultIsError == "true"
		callCount := 0
		streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			callCount++
			if callCount == 1 {
				return &mockStream{chunks: []*llm.Chunk{{
					Calls: []llm.Call{{
						ID: "call-1",
						Function: struct {
							Name      string `json:"name"`
							Arguments string `json:"arguments"`
						}{Name: "test_tool", Arguments: `{}`},
					}},
					StopReason: "toolUse",
				}}}, nil
			}
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "done", StopReason: llm.StopReason(stopReason)},
			}}, nil
		}

		tl := Tool{
			Name: "test_tool",
			Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
				return session.ToolResultMessage{
					ToolCallID: id,
					ToolName:   "test_tool",
					Content:    []session.Content{session.TextContent{Text: toolContent}},
					IsError:    isError,
					Timestamp:  time.Now(),
				}, nil
			},
		}

		cfg := LoopConfig{
			Model:    llm.Model{ID: modelID},
			StreamFn: streamFn,
			Tools:    []Tool{tl},
			Convert:  DefaultConvert,
		}

		msgs := RunLoop(
			context.Background(),
			[]session.Message{session.NewUserText("prompt", time.Now())},
			TurnContext{}, cfg, func(e session.Event) {}, nil,
		)
		_ = msgs
	})
}

// FuzzConvert tests message conversion against invalid/truncated messages.
func FuzzConvert(f *testing.F) {
	f.Add("tool_result", `content text`, false)
	f.Fuzz(func(t *testing.T, msgType string, content string, isError bool) {
		defer func() {
			if r := recover(); r != nil {
				t.Fatalf("DefaultConvert panicked: %v", r)
			}
		}()

		var msg session.Message
		switch msgType {
		case "user":
			msg = session.NewUserText(content, time.Now())
		case "assistant":
			msg = &session.AssistantMessage{
				Content:    []session.Content{session.TextContent{Text: content}},
				StopReason: session.StopReasonEndTurn,
				Timestamp:  time.Now(),
			}
		case "tool_result":
			msg = &session.ToolResultMessage{
				ToolCallID: "call-1",
				ToolName:   "test",
				Content:    []session.Content{session.TextContent{Text: content}},
				IsError:    isError,
				Timestamp:  time.Now(),
			}
		default:
			// Unknown type — skip.
			return
		}

		result := DefaultConvert([]session.Message{msg})
		_ = result
	})
}
