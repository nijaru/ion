package anthropic

import (
	"context"

	sdk "github.com/anthropics/anthropic-sdk-go"
	"github.com/nijaru/ion/llm"
)

// Stream implements llm.Stream for Anthropic.
type Stream struct {
	// The SDK returns a pointer to a struct from an internal package.
	// We use the same type returned by MessageService.NewStreaming.
	stream interface {
		Next() bool
		Current() sdk.MessageStreamEventUnion
		Err() error
		Close() error
	}
	err        error
	activeCall *llm.Call
	targetName string
	model      string
	p          *Provider
	ctx        context.Context
	usage      llm.Usage
}

func (s *Stream) Next() (*llm.Chunk, bool) {
	for s.stream.Next() {
		event := s.stream.Current()

		switch event.Type {
		case "message_start":
			msg := event.AsMessageStart()
			return s.updateUsage(usageFromMessage(msg.Message.Usage)), true
		case "content_block_start":
			chunk := s.contentBlockStart(event.AsContentBlockStart())
			if chunk != nil {
				return chunk, true
			}
		case "content_block_delta":
			chunk := s.contentBlockDelta(event.AsContentBlockDelta())
			if chunk != nil {
				return chunk, true
			}
		case "message_delta":
			delta := event.AsMessageDelta()
			return s.updateUsage(usageFromMessageDelta(delta.Usage)), true
		case "content_block_stop":
			s.activeCall = nil
		case "message_stop":
			return nil, false
		}
	}

	if err := s.stream.Err(); err != nil {
		s.err = err
	}
	return nil, false
}

func (s *Stream) contentBlockStart(start sdk.ContentBlockStartEvent) *llm.Chunk {
	switch start.ContentBlock.Type {
	case "tool_use":
		s.activeCall = &llm.Call{
			ID:   start.ContentBlock.ID,
			Type: "function",
		}
		s.activeCall.Function.Name = start.ContentBlock.Name
		return &llm.Chunk{Calls: []llm.Call{*s.activeCall}}
	case "thinking":
		return &llm.Chunk{
			Reasoning: start.ContentBlock.Thinking,
			ThinkingBlocks: []llm.ThinkingBlock{{
				Thinking:  start.ContentBlock.Thinking,
				Signature: start.ContentBlock.Signature,
			}},
		}
	case "redacted_thinking":
		return &llm.Chunk{
			Reasoning: "<redacted_thinking />",
			ThinkingBlocks: []llm.ThinkingBlock{{
				Redacted:  true,
				Signature: start.ContentBlock.Signature,
			}},
		}
	default:
		return nil
	}
}

func (s *Stream) contentBlockDelta(delta sdk.ContentBlockDeltaEvent) *llm.Chunk {
	switch delta.Delta.Type {
	case "text_delta":
		return &llm.Chunk{Content: delta.Delta.Text}
	case "thinking_delta":
		return &llm.Chunk{
			Reasoning: delta.Delta.Thinking,
			ThinkingBlocks: []llm.ThinkingBlock{{
				Thinking: delta.Delta.Thinking,
			}},
		}
	case "input_json_delta":
		if s.activeCall == nil {
			return nil
		}
		s.activeCall.Function.Arguments += delta.Delta.PartialJSON
		chunk := &llm.Chunk{Calls: []llm.Call{*s.activeCall}}
		if s.activeCall.Function.Name == s.targetName {
			chunk.Content = delta.Delta.PartialJSON
		}
		return chunk
	default:
		return nil
	}
}

func (s *Stream) updateUsage(next llm.Usage) *llm.Chunk {
	s.usage = next
	s.usage.TotalTokens = s.usage.InputTokens + s.usage.OutputTokens
	s.usage.Cost = s.p.Cost(s.ctx, s.model, s.usage)
	usage := s.usage
	return &llm.Chunk{Usage: &usage}
}

func usageFromMessage(usage sdk.Usage) llm.Usage {
	return llm.Usage{
		InputTokens:         int(usage.InputTokens),
		OutputTokens:        int(usage.OutputTokens),
		CacheReadTokens:     int(usage.CacheReadInputTokens),
		CacheCreationTokens: int(usage.CacheCreationInputTokens),
	}
}

func usageFromMessageDelta(usage sdk.MessageDeltaUsage) llm.Usage {
	return llm.Usage{
		InputTokens:         int(usage.InputTokens),
		OutputTokens:        int(usage.OutputTokens),
		CacheReadTokens:     int(usage.CacheReadInputTokens),
		CacheCreationTokens: int(usage.CacheCreationInputTokens),
	}
}

func (s *Stream) Err() error {
	return s.err
}

func (s *Stream) Close() error {
	return s.stream.Close()
}
