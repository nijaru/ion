package openrouter

import (
	"bufio"
	"context"
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
	provider    *Provider
	ctx         context.Context
	model       string
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
		if !strings.HasPrefix(line, "data:") {
			continue
		}

		data := strings.TrimSpace(strings.TrimPrefix(line, "data:"))
		if data == "[DONE]" {
			return nil, false
		}

		var resp sashaoai.ChatCompletionStreamResponse
		if err := json.Unmarshal([]byte(data), &resp); err != nil {
			s.err = err
			return nil, false
		}

		// Handle final usage chunk (which may have no choices).
		if resp.Usage != nil {
			usage := llm.Usage{
				InputTokens:  resp.Usage.PromptTokens,
				OutputTokens: resp.Usage.CompletionTokens,
				TotalTokens:  resp.Usage.TotalTokens,
			}
			if rawCost := openRouterUsageCost(data); rawCost > 0 {
				usage.Cost = rawCost
			} else if s.provider != nil {
				usage.Cost = s.provider.Base.Cost(s.ctx, s.model, usage)
			}
			return &llm.Chunk{
				Model:         resp.Model,
				ResponseModel: responseModel(resp.Model, s.model),
				ResponseID:    resp.ID,
				Usage:         &usage,
			}, true
		}

		if len(resp.Choices) == 0 {
			continue
		}

		choice := resp.Choices[0]
		chunk := &llm.Chunk{
			Content:       choice.Delta.Content,
			Reasoning:     choice.Delta.ReasoningContent,
			Model:         resp.Model,
			ResponseModel: responseModel(resp.Model, s.model),
			ResponseID:    resp.ID,
		}
		if choice.FinishReason != "" {
			chunk.StopReason = mapFinishReason(string(choice.FinishReason))
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

func openRouterUsageCost(data string) float64 {
	var envelope struct {
		Usage *struct {
			Cost float64 `json:"cost"`
		} `json:"usage"`
	}
	if err := json.Unmarshal([]byte(data), &envelope); err != nil || envelope.Usage == nil {
		return 0
	}
	return envelope.Usage.Cost
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
