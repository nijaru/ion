package openaicodex

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/nijaru/ion/llm"
)

type stream struct {
	body          io.ReadCloser
	scanner       *bufio.Scanner
	model         string
	eventType     string
	data          []string
	done          bool
	terminal      bool
	err           error
	closed        bool
	calls         map[int]callState
	reasoningSeen map[int]bool
}

type callState struct {
	id        string
	itemID    string
	name      string
	arguments string
}

func newStream(body io.ReadCloser, model string) *stream {
	scanner := bufio.NewScanner(body)
	scanner.Buffer(make([]byte, 0, 64<<10), 2<<20)
	return &stream{
		body:          body,
		scanner:       scanner,
		model:         model,
		calls:         make(map[int]callState),
		reasoningSeen: make(map[int]bool),
	}
}

func (s *stream) Next() (*llm.Chunk, bool) {
	if s.done || s.err != nil || s.closed {
		return nil, false
	}
	for {
		event, ok := s.nextEvent()
		if !ok {
			if s.err == nil && !s.terminal {
				s.err = errors.New("openai-codex: stream ended before a terminal response event")
			}
			s.done = true
			return nil, false
		}
		chunk := s.chunkForEvent(event)
		if chunk != nil {
			return chunk, true
		}
		if s.done || s.err != nil {
			return nil, false
		}
	}
}

func (s *stream) nextEvent() (map[string]any, bool) {
	for s.scanner.Scan() {
		line := strings.TrimSuffix(s.scanner.Text(), "\r")
		if line == "" {
			if len(s.data) == 0 {
				continue
			}
			payload := strings.TrimSpace(strings.Join(s.data, "\n"))
			s.eventType, s.data = "", nil
			if payload == "[DONE]" {
				if !s.terminal {
					s.err = errors.New("openai-codex: stream ended before a terminal response event")
				}
				s.done = true
				return nil, false
			}
			var event map[string]any
			if err := json.Unmarshal([]byte(payload), &event); err != nil {
				s.err = fmt.Errorf("openai-codex: decode SSE event: %w", err)
				return nil, false
			}
			if s.eventType != "" {
				event["_event"] = s.eventType
			}
			return event, true
		}
		switch {
		case strings.HasPrefix(line, "event:"):
			s.eventType = strings.TrimSpace(strings.TrimPrefix(line, "event:"))
		case strings.HasPrefix(line, "data:"):
			s.data = append(s.data, strings.TrimSpace(strings.TrimPrefix(line, "data:")))
		}
	}
	if err := s.scanner.Err(); err != nil {
		s.err = err
	}
	return nil, false
}

func (s *stream) chunkForEvent(event map[string]any) *llm.Chunk {
	typeName, _ := event["type"].(string)
	switch typeName {
	case "response.created":
		return &llm.Chunk{Model: s.model, ResponseID: stringValue(nested(event, "response", "id"))}
	case "response.output_text.delta", "response.refusal.delta":
		return &llm.Chunk{Content: stringValue(event["delta"]), Model: s.model}
	case "response.reasoning_summary_text.delta", "response.reasoning_text.delta":
		s.reasoningSeen[intValue(event["output_index"])] = true
		return &llm.Chunk{Reasoning: stringValue(event["delta"]), Model: s.model}
	case "response.output_item.added":
		return s.addedToolCall(event)
	case "response.function_call_arguments.delta":
		return s.toolCallDelta(event)
	case "response.function_call_arguments.done":
		return s.toolCallDone(event)
	case "response.output_item.done":
		item := mapValue(event["item"])
		switch stringValue(item["type"]) {
		case "function_call":
			return s.toolCallDone(event)
		case "reasoning":
			return s.reasoningDone(event)
		default:
			return nil
		}
	case "response.completed", "response.done", "response.incomplete":
		response := mapValue(event["response"])
		status := stringValue(response["status"])
		if typeName == "response.incomplete" || status == "incomplete" {
			details := mapValue(response["incomplete_details"])
			reason := stringValue(details["reason"])
			if reason != "max_output_tokens" {
				s.err = fmt.Errorf("openai-codex: incomplete response: %s", reason)
				s.done = true
				return nil
			}
		}
		s.terminal = true
		s.done = true
		chunk := &llm.Chunk{
			Model:      s.model,
			ResponseID: stringValue(response["id"]),
			StopReason: stopReason(status, response),
		}
		if usage := responseUsage(response["usage"]); usage != nil {
			chunk.Usage = usage
		}
		return chunk
	case "error", "response.failed":
		s.err = errors.New(codexEventError(event))
		s.done = true
	}
	return nil
}

func (s *stream) addedToolCall(event map[string]any) *llm.Chunk {
	item := mapValue(event["item"])
	if stringValue(item["type"]) != "function_call" {
		return nil
	}
	index := intValue(event["output_index"])
	state := callState{
		id:        stringValue(item["call_id"]),
		itemID:    stringValue(item["id"]),
		name:      stringValue(item["name"]),
		arguments: stringValue(item["arguments"]),
	}
	s.calls[index] = state
	return &llm.Chunk{Calls: []llm.Call{callFromState(state)}, Model: s.model}
}

func (s *stream) toolCallDelta(event map[string]any) *llm.Chunk {
	index := intValue(event["output_index"])
	state := s.calls[index]
	state.arguments += stringValue(event["delta"])
	s.calls[index] = state
	return &llm.Chunk{Calls: []llm.Call{callFromState(state)}, Model: s.model}
}

func (s *stream) reasoningDone(event map[string]any) *llm.Chunk {
	item := mapValue(event["item"])
	encoded, err := json.Marshal(item)
	if err != nil {
		return nil
	}
	thinking := ""
	if !s.reasoningSeen[intValue(event["output_index"])] {
		if summary, ok := item["summary"].([]any); ok {
			for _, part := range summary {
				thinking += stringValue(mapValue(part)["text"])
			}
		}
	}
	return &llm.Chunk{
		ThinkingBlocks: []llm.ThinkingBlock{{Thinking: thinking, Signature: string(encoded)}},
		Model:          s.model,
	}
}

func (s *stream) toolCallDone(event map[string]any) *llm.Chunk {
	item := mapValue(event["item"])
	itemType := stringValue(item["type"])
	if itemType != "" && itemType != "function_call" {
		return nil
	}
	index := intValue(event["output_index"])
	state := s.calls[index]
	if value := stringValue(item["call_id"]); value != "" {
		state.id = value
	}
	if value := stringValue(item["id"]); value != "" {
		state.itemID = value
	}
	if value := stringValue(item["name"]); value != "" {
		state.name = value
	}
	if value := stringValue(item["arguments"]); value != "" {
		state.arguments = value
	}
	if value := stringValue(event["arguments"]); value != "" {
		state.arguments = value
	}
	s.calls[index] = state
	return &llm.Chunk{Calls: []llm.Call{callFromState(state)}, Model: s.model}
}

func callFromState(state callState) llm.Call {
	id := state.id
	if state.itemID != "" {
		id += "|" + state.itemID
	}
	return llm.Call{ID: id, Type: "function", Function: struct {
		Name      string `json:"name"`
		Arguments string `json:"arguments"`
	}{Name: state.name, Arguments: state.arguments}}
}

func (s *stream) Err() error { return s.err }

func (s *stream) Close() error {
	if s.closed {
		return nil
	}
	s.closed = true
	return s.body.Close()
}

func mapValue(value any) map[string]any {
	if value, ok := value.(map[string]any); ok {
		return value
	}
	return map[string]any{}
}

func nested(value map[string]any, keys ...string) any {
	current := value
	for _, key := range keys[:len(keys)-1] {
		current = mapValue(current[key])
	}
	return current[keys[len(keys)-1]]
}

func stringValue(value any) string {
	result, _ := value.(string)
	return result
}

func intValue(value any) int {
	number, _ := value.(float64)
	return int(number)
}

func responseUsage(value any) *llm.Usage {
	usage := mapValue(value)
	if len(usage) == 0 {
		return nil
	}
	input := intValue(usage["input_tokens"])
	output := intValue(usage["output_tokens"])
	total := intValue(usage["total_tokens"])
	return &llm.Usage{InputTokens: input, OutputTokens: output, TotalTokens: total}
}

func stopReason(status string, response map[string]any) llm.StopReason {
	if status == "incomplete" {
		details := mapValue(response["incomplete_details"])
		if stringValue(details["reason"]) == "max_output_tokens" {
			return llm.StopReasonLength
		}
	}
	if output, ok := response["output"].([]any); ok {
		for _, raw := range output {
			if stringValue(mapValue(raw)["type"]) == "function_call" {
				return llm.StopReasonToolUse
			}
		}
	}
	return llm.StopReasonStop
}

func codexEventError(event map[string]any) string {
	if message := stringValue(event["message"]); message != "" {
		return "openai-codex: " + message
	}
	errorValue := mapValue(event["error"])
	if message := stringValue(errorValue["message"]); message != "" {
		return "openai-codex: " + message
	}
	return "openai-codex: provider request failed"
}

var _ llm.Stream = (*stream)(nil)
