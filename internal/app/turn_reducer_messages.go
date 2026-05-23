package app

import (
	"strings"
	"time"

	"github.com/nijaru/ion/internal/session"
)

func (r turnReducer) appendThinkingDelta(agentID, delta string) {
	if agentID == "" {
		if r.inFlight.AgentCommitted {
			return
		}
		r.inFlight.ReasonBuf += delta
		return
	}
	if p, ok := r.inFlight.Subagents[agentID]; ok {
		p.Reasoning += delta
	}
}

func (r turnReducer) appendAgentDelta(agentID, delta string, timestamp time.Time) {
	if agentID == "" {
		if r.inFlight.AgentCommitted {
			return
		}
		r.progress.Mode = stateStreaming
		if r.inFlight.Pending == nil || r.inFlight.Pending.Role != session.Agent {
			r.inFlight.Pending = &session.Entry{
				Role:      session.Agent,
				Timestamp: timestamp,
			}
		}
		r.inFlight.StreamChunks = append(r.inFlight.StreamChunks, delta)
		if r.inFlight.StreamBuf == "" {
			r.inFlight.StreamBuf = delta
		}
		if r.inFlight.Pending.Content == "" {
			r.inFlight.Pending.Content = delta
		}
		return
	}
	if p, ok := r.inFlight.Subagents[agentID]; ok {
		p.Output += delta
	}
}

func (r turnReducer) commitAgentMessage(msg session.AgentMessage) (session.Entry, bool) {
	if msg.AgentID != "" {
		return session.Entry{}, false
	}
	if r.inFlight.Pending != nil && r.inFlight.Pending.Role == session.Agent {
		if msg.Message != "" {
			r.inFlight.Pending.Content = msg.Message
		} else if streamContent := r.agentStreamContent(); streamContent != "" {
			r.inFlight.Pending.Content = streamContent
		}
		r.inFlight.Pending.Reasoning = r.inFlight.ReasonBuf
		if msg.Reasoning != "" {
			r.inFlight.Pending.Reasoning = msg.Reasoning
		}
		setEntryTimestamp(r.inFlight.Pending, msg.Timestamp)
		entry := *r.inFlight.Pending
		r.clearPendingAssistant()
		if strings.TrimSpace(entry.Content) == "" && strings.TrimSpace(entry.Reasoning) == "" {
			return session.Entry{}, false
		}
		r.inFlight.AgentCommitted = true
		return entry, true
	}
	entry := session.Entry{
		Role:      session.Agent,
		Timestamp: msg.Timestamp,
		Content:   msg.Message,
		Reasoning: r.inFlight.ReasonBuf,
	}
	if msg.Reasoning != "" {
		entry.Reasoning = msg.Reasoning
	}
	r.inFlight.StreamBuf = ""
	r.inFlight.StreamChunks = nil
	r.inFlight.ReasonBuf = ""
	if strings.TrimSpace(entry.Content) == "" && strings.TrimSpace(entry.Reasoning) == "" {
		return session.Entry{}, false
	}
	r.inFlight.AgentCommitted = true
	return entry, true
}

func (r turnReducer) agentStreamContent() string {
	if len(r.inFlight.StreamChunks) > 0 {
		return strings.Join(r.inFlight.StreamChunks, "")
	}
	if r.inFlight.Pending != nil && r.inFlight.Pending.Role == session.Agent {
		return r.inFlight.Pending.Content
	}
	return r.inFlight.StreamBuf
}

func (r turnReducer) agentStreamEmpty() bool {
	return len(r.inFlight.StreamChunks) == 0 &&
		r.inFlight.StreamBuf == "" &&
		(r.inFlight.Pending == nil || r.inFlight.Pending.Content == "")
}
