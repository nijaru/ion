package openrouter

import (
	"bufio"
	"encoding/json"
	"io"
	"strings"

	"github.com/nijaru/ion/llm"
	sashaoai "github.com/sashabaranov/go-openai"
)

// openRouterStream implements llm.Stream for OpenRouter by parsing SSE events
// directly from the HTTP response body.
type openRouterStream struct {
	body        io.Closer
	reader      io.Reader
	scanner     *bufio.Scanner
	activeCalls map[int]llm.Call
	err         error
}

func (s *openRouterStream) Next() (*llm.Chunk, bool) {
	if s.scanner == nil {
		s.scanner = bufio.NewScanner(s.reader)
		s.activeCalls = make(map[int]llm.Call)
		s.scanner.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	}

	for {
		if !s.scanner.Scan() {
			if err := s.scanner.Err(); err != nil {
				s.err = err
			}
			return nil, false
		}

		line := strings.TrimSpace(s.scanner.Text())
		if line == "" {
			continue
		}
		if !strings.HasPrefix(line, "data: ") {
			continue
		}

		data := strings.TrimPrefix(line, "data: ")
		if data == "[DONE]" {
			return nil, false
		}

		var resp sashaoai.ChatCompletionStreamResponse
		if err := json.Unmarshal([]byte(data), &resp); err != nil {
			continue
		}

		// Handle final usage chunk (which may have no choices).
		if resp.Usage != nil {
			return &llm.Chunk{
				Usage: &llm.Usage{
					InputTokens:  resp.Usage.PromptTokens,
					OutputTokens: resp.Usage.CompletionTokens,
					TotalTokens:  resp.Usage.TotalTokens,
				},
			}, true
		}

		if len(resp.Choices) == 0 {
			continue
		}

		choice := resp.Choices[0]
		chunk := &llm.Chunk{
			Content:   choice.Delta.Content,
			Reasoning: choice.Delta.ReasoningContent,
		}

		if len(choice.Delta.ToolCalls) > 0 {
			chunk.Calls = make([]llm.Call, len(choice.Delta.ToolCalls))
			for i, delta := range choice.Delta.ToolCalls {
				index := delta.Index
				if index == nil {
					idx := i
					index = &idx
				}

				call, ok := s.activeCalls[*index]
				if !ok {
					call = llm.Call{Type: string(delta.Type)}
				}

				if delta.ID != "" {
					call.ID = delta.ID
				}
				if delta.Function.Name != "" {
					call.Function.Name = delta.Function.Name
				}
				if delta.Function.Arguments != "" {
					call.Function.Arguments += delta.Function.Arguments
				}

				s.activeCalls[*index] = call
				chunk.Calls[i] = call
			}
		}

		return chunk, true
	}
}

func (s *openRouterStream) Err() error {
	return s.err
}

func (s *openRouterStream) Close() error {
	if s.body != nil {
		return s.body.Close()
	}
	return nil
}

var _ llm.Stream = (*openRouterStream)(nil)
