package openai

import (
	"errors"
	"io"

	"github.com/nijaru/ion/internal/llm"
	"github.com/sashabaranov/go-openai"
)

// OpenAIStream implements llm.Stream for OpenAI-compatible providers.
type OpenAIStream struct {
	stream      *openai.ChatCompletionStream
	err         error
	activeCalls map[int]llm.Call // Track partial calls by their index in the response
}

func (s *OpenAIStream) Next() (*llm.Chunk, bool) {
	for {
		resp, err := s.stream.Recv()
		if err != nil {
			if errors.Is(err, io.EOF) {
				return nil, false
			}
			s.err = err
			return nil, false
		}

		// Handle final usage chunk (which may have no choices)
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
					// Fallback if index is missing (unlikely in modern OpenAI)
					idx := i
					index = &idx
				}

				call, ok := s.activeCalls[*index]
				if !ok {
					call = llm.Call{
						Type: string(delta.Type),
					}
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

func (s *OpenAIStream) Err() error {
	return s.err
}

func (s *OpenAIStream) Close() error {
	s.stream.Close()
	return nil
}
