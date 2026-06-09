package agent

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/llm"
)

func (s *SessionAdapter) appendModelMessage(ctx context.Context, message llm.Message) error {
	s.mu.Lock()
	sess := s.sess
	s.mu.Unlock()
	if sess == nil {
		return nil
	}
	return sess.AppendModelMessage(ctx, message)
}

func (s *SessionAdapter) loadModelHistoryLocked(ctx context.Context) ([]AgentMessage, error) {
	if s.sess == nil {
		return nil, nil
	}
	messages, err := s.sess.ModelMessages(ctx)
	if err != nil {
		return nil, fmt.Errorf("load model history: %w", err)
	}
	result := make([]AgentMessage, 0, len(messages))
	for _, message := range messages {
		result = append(result, agentMessageFromLLM(message))
	}
	return result, nil
}

// trimLastAssistantMessage removes the last assistant message from agent state.
// Used during overflow recovery to remove the error message before retrying.
// Caller must hold s.mu.
func (s *SessionAdapter) trimLastAssistantMessage() {
	msgs := s.agent.state.Messages
	if len(msgs) > 0 && msgs[len(msgs)-1].Role == "assistant" {
		s.agent.state.Messages = msgs[:len(msgs)-1]
	}
}

// updateContextTokens updates the estimated context token count.
// Called from TokenUsage events.
// Caller must hold s.mu.
func (s *SessionAdapter) updateContextTokens(input, output int) {
	s.contextTokens += input + output
}

// needsCompaction checks if context tokens exceed the threshold.
// Returns true if compaction should be triggered.
// Caller must hold s.mu.
func (s *SessionAdapter) needsCompaction() bool {
	if s.config == nil {
		return false
	}
	contextWindow := s.config.Model.ContextWindow
	if contextWindow <= 0 {
		return false
	}
	// Use 80% threshold (matching Pi's default)
	threshold := int(float64(contextWindow) * 0.8)
	return s.contextTokens > threshold
}

// resetContextTokens resets the context token counter.
// Called after successful compaction.
// Caller must hold s.mu.
func (s *SessionAdapter) resetContextTokens() {
	s.contextTokens = 0
}

// runCompaction runs the compaction function if available.
// Caller must NOT hold s.mu (blocking call).
func (s *SessionAdapter) runCompaction(ctx context.Context) (bool, error) {
	s.mu.Lock()
	compactFn := s.config.CompactFunc
	closed := s.closed
	s.mu.Unlock()

	if closed {
		return false, fmt.Errorf("session is closed")
	}
	if compactFn == nil {
		return false, nil
	}
	return compactFn(ctx)
}
