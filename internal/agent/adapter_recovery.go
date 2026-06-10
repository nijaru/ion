package agent

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/nijaru/ion/session"
)

// handlePostAgentRun handles post-agent-run logic including overflow
// recovery and auto-retry with exponential backoff.
// Emits AgentEnd when all recovery is exhausted (Pi parity).
func (s *SessionAdapter) handlePostAgentRun(ctx context.Context, err error, newMessages []AgentMessage) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.closed {
		return
	}

	// Convert domain messages to session messages for event payloads.
	sessionMsgs := toSessionAgentMessages(newMessages)

	// Success or cancellation — agent already emitted TurnEnd
	if err == nil || errors.Is(err, context.Canceled) {
		s.retryAttempt = 0
		s.emitEvent(session.AgentEnd{Base: session.BaseNow(), Messages: sessionMsgs})
		return
	}

	errMsg := err.Error()
	agentErr := NewAgentError(errMsg, err)

	// Overflow recovery: compact and retry once
	if agentErr.Code == ErrCodeOverflow && !s.overflowAttempted {
		s.overflowAttempted = true
		if s.recoverFromOverflow(ctx) {
			return
		}
	}

	// Transient error retry with exponential backoff
	if agentErr.IsRetryable && s.retryAttempt < s.config.GetMaxRetries() {
		if s.retryWithBackoff(ctx, errMsg) {
			return
		}
	}

	// Non-retryable error — agent already emitted TurnEnd{Error}
	s.emitEvent(session.AgentEnd{Base: session.BaseNow(), Error: err, Messages: sessionMsgs})
}

// recoverFromOverflow handles context overflow by compacting and retrying.
// Returns true if recovery succeeded or was attempted. Caller must hold s.mu.
func (s *SessionAdapter) recoverFromOverflow(ctx context.Context) bool {
	s.emitEvent(session.CompactionTrigger{
		Base:   session.BaseNow(),
		Reason: "overflow",
	})
	s.trimLastAssistantMessage()

	// Unlock for blocking compaction call
	s.mu.Unlock()
	defer s.mu.Lock()

	compacted, err := s.runCompaction(ctx)
	if err != nil {
		if !s.closed {
			s.emitEvent(session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				FinalError: fmt.Sprintf("compaction failed: %v", err),
			})
			s.emitEvent(session.AgentEnd{Base: session.BaseNow()})
		}
		return true
	}
	if compacted {
		s.resetContextTokens()
	}

	newMessages, retryErr := s.agent.Continue(ctx)
	if !s.closed {
		sessionMsgs := toSessionAgentMessages(newMessages)
		if retryErr != nil && !errors.Is(retryErr, context.Canceled) {
			s.emitEvent(session.AgentEnd{Base: session.BaseNow(), Error: retryErr, Messages: sessionMsgs})
		} else {
			s.emitEvent(session.AgentEnd{Base: session.BaseNow(), Messages: sessionMsgs})
		}
	}
	return true
}

// retryWithBackoff retries a failed turn with exponential backoff.
// Returns true if retry was attempted. Caller must hold s.mu.
func (s *SessionAdapter) retryWithBackoff(ctx context.Context, errMsg string) bool {
	s.retryAttempt++
	delayMs := s.config.GetRetryBaseDelayMs() * (1 << (s.retryAttempt - 1))

	s.emitEvent(session.AutoRetryStart{
		Base:       session.BaseNow(),
		Attempt:    s.retryAttempt,
		MaxAttempt: s.config.GetMaxRetries(),
		DelayMs:    delayMs,
		Error:      errMsg,
	})
	s.trimLastAssistantMessage()

	// Unlock for blocking delay
	s.mu.Unlock()
	select {
	case <-ctx.Done():
		s.mu.Lock()
		if !s.closed {
			s.emitEvent(session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				Attempt:    s.retryAttempt,
				FinalError: "Retry cancelled",
			})
			s.emitEvent(session.AgentEnd{Base: session.BaseNow()})
		}
		return true
	case <-time.After(time.Duration(delayMs) * time.Millisecond):
	}
	s.mu.Lock()

	if s.closed {
		return true
	}

	// Retry the turn (unlock for blocking call)
	s.mu.Unlock()
	newMessages, retryErr := s.agent.Continue(ctx)
	s.mu.Lock()

	if s.closed {
		return true
	}

	sessionMsgs := toSessionAgentMessages(newMessages)
	if retryErr == nil || errors.Is(retryErr, context.Canceled) {
		s.emitEvent(session.AutoRetryEnd{
			Base:    session.BaseNow(),
			Success: true,
			Attempt: s.retryAttempt,
		})
		s.retryAttempt = 0
		s.emitEvent(session.AgentEnd{Base: session.BaseNow(), Messages: sessionMsgs})
		return true
	}

	// Retry failed — handle the new error (may retry again)
	handlePostAgentRunErr := retryErr
	_ = handlePostAgentRunErr // handled below after mu is re-acquired
	// Note: handlePostAgentRun acquires mu, so we must release first
	s.mu.Unlock()
	s.handlePostAgentRun(ctx, retryErr, newMessages)
	s.mu.Lock()
	return true
}
