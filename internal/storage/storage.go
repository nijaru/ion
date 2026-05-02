package storage

import (
	"context"
	"strings"
	"time"

	"github.com/nijaru/ion/internal/session"
)

// Store defines the interface for session and input history persistence.
type Store interface {
	// OpenSession initializes a new session and its storage file.
	OpenSession(ctx context.Context, cwd, model, branch string) (Session, error)

	// ResumeSession loads an existing session by ID.
	ResumeSession(ctx context.Context, id string) (Session, error)

	// ListSessions returns a list of sessions for the given working directory.
	ListSessions(ctx context.Context, cwd string) ([]SessionInfo, error)

	// GetRecentSession returns the most recently updated session for the given directory.
	GetRecentSession(ctx context.Context, cwd string) (*SessionInfo, error)

	// AddInput appends a user input string to the directory's history.
	AddInput(ctx context.Context, cwd, content string) error

	// GetInputs returns the input history for the given directory.
	GetInputs(ctx context.Context, cwd string, limit int) ([]string, error)

	// UpdateSession updates the session's index metadata.
	UpdateSession(ctx context.Context, si SessionInfo) error

	// Close finalizes and closes any open storage resources.
	Close() error
}

type ForkOptions struct {
	Label  string
	Reason string
}

type SessionForker interface {
	ForkSession(ctx context.Context, parentID string, opts ForkOptions) (Session, error)
}

type SessionTree struct {
	Current  SessionInfo
	Lineage  []SessionInfo
	Children []SessionInfo
}

type SessionTreeReader interface {
	SessionTree(ctx context.Context, sessionID string) (SessionTree, error)
}

// Session handles appending events to a specific session's storage.
type Session interface {
	// ID returns the unique session identifier.
	ID() string

	// Meta returns the session's initial metadata.
	Meta() Metadata

	// Append appends a new event to the session storage.
	Append(ctx context.Context, event any) error

	// Entries returns all entries stored in the session so far.
	Entries(ctx context.Context) ([]session.Entry, error)

	// LastStatus returns the most recent status persisted in the session.
	LastStatus(ctx context.Context) (string, error)

	// Usage returns the total tokens and cost accumulated in the session.
	Usage(ctx context.Context) (int, int, float64, error)

	// Close finalizes and closes any open file handles for the session.
	Close() error
}

// Metadata represents the persistent meta for a session.
type Metadata struct {
	ID        string    `json:"id"`
	CWD       string    `json:"cwd"`
	Model     string    `json:"model"`
	Branch    string    `json:"branch"`
	CreatedAt time.Time `json:"created_at"`
}

// SessionInfo provides summary information about a session for lists and pickers.
type SessionInfo struct {
	ID           string    `json:"id"`
	CWD          string    `json:"cwd"`
	Model        string    `json:"model"`
	Branch       string    `json:"branch"`
	CreatedAt    time.Time `json:"created_at"`
	UpdatedAt    time.Time `json:"updated_at"`
	MessageCount int       `json:"message_count"`
	Title        string    `json:"title"`
	Summary      string    `json:"summary"`
	LastPreview  string    `json:"last_preview"`
}

func IsConversationSessionInfo(info SessionInfo) bool {
	title := strings.TrimSpace(info.Title)
	if strings.HasPrefix(title, "/") {
		return false
	}
	preview := strings.TrimSpace(info.LastPreview)
	return preview != "" && !strings.HasPrefix(preview, "/")
}

// JSONL entry types as defined in ai/design/session-storage.md
type (
	Meta struct {
		Type      string `json:"type"` // "meta"
		ID        string `json:"id"`
		CWD       string `json:"cwd"`
		Model     string `json:"model"`
		Branch    string `json:"branch"`
		CreatedAt int64  `json:"created_at"`
	}

	User struct {
		Type    string `json:"type"` // "user"
		Content string `json:"content"`
		TS      int64  `json:"ts"`
	}

	Status struct {
		Type   string `json:"type"` // "status"
		Status string `json:"status"`
		TS     int64  `json:"ts"`
	}

	System struct {
		Type    string `json:"type"` // "system"
		Content string `json:"content"`
		TS      int64  `json:"ts"`
	}

	Agent struct {
		Type    string  `json:"type"` // "agent"
		Content []Block `json:"content"`
		TS      int64   `json:"ts"`
	}

	ToolUse struct {
		Type  string `json:"type"` // "tool_use"
		ID    string `json:"id"`
		Name  string `json:"name"`
		Input any    `json:"input"`
		TS    int64  `json:"ts"`
	}

	ToolResult struct {
		Type      string `json:"type"` // "tool_result"
		ToolUseID string `json:"tool_use_id"`
		Content   string `json:"content"`
		IsError   bool   `json:"is_error"`
		TS        int64  `json:"ts"`
	}

	TokenUsage struct {
		Type   string  `json:"type"` // "token_usage"
		Input  int     `json:"input"`
		Output int     `json:"output"`
		Cost   float64 `json:"cost"`
		TS     int64   `json:"ts"`
	}

	RoutingDecision struct {
		Type           string  `json:"type"` // "routing_decision"
		Decision       string  `json:"decision"`
		Reason         string  `json:"reason"`
		ModelSlot      string  `json:"model_slot"`
		Provider       string  `json:"provider"`
		Model          string  `json:"model"`
		Reasoning      string  `json:"reasoning,omitempty"`
		MaxSessionCost float64 `json:"max_session_cost,omitempty"`
		MaxTurnCost    float64 `json:"max_turn_cost,omitempty"`
		SessionCost    float64 `json:"session_cost,omitempty"`
		TurnCost       float64 `json:"turn_cost,omitempty"`
		StopReason     string  `json:"stop_reason,omitempty"`
		TS             int64   `json:"ts"`
	}

	EscalationNotification struct {
		Type      string `json:"type"` // "escalation_notification"
		RequestID string `json:"request_id"`
		Channel   string `json:"channel"`
		Target    string `json:"target"`
		Status    string `json:"status"` // "sent", "failed", "skipped"
		Detail    string `json:"detail,omitempty"`
		TS        int64  `json:"ts"`
	}

	Subagent struct {
		Type    string `json:"type"` // "subagent"
		Name    string `json:"name"`
		Content string `json:"content"`
		IsError bool   `json:"is_error"`
		TS      int64  `json:"ts"`
	}

	Block struct {
		Type     string  `json:"type"` // "text" or "thinking"
		Text     *string `json:"text,omitempty"`
		Thinking *string `json:"thinking,omitempty"`
	}
)

func agentMessagePayload(e Agent) (string, string) {
	var content strings.Builder
	var reasoning strings.Builder
	for _, b := range e.Content {
		if b.Type == "text" && b.Text != nil {
			content.WriteString(*b.Text)
		}
		if b.Type == "thinking" && b.Thinking != nil {
			reasoning.WriteString(*b.Thinking)
		}
	}
	return content.String(), reasoning.String()
}

func hasAgentMessagePayload(content, reasoning string) bool {
	return strings.TrimSpace(content) != "" || strings.TrimSpace(reasoning) != ""
}

func isNoopAppendEvent(event any) bool {
	e, ok := event.(Agent)
	if !ok {
		return false
	}
	content, reasoning := agentMessagePayload(e)
	return !hasAgentMessagePayload(content, reasoning)
}

func sessionTitle(text string) string {
	return compactSessionText(text, 72)
}

func sessionSummary(text string) string {
	return compactSessionText(text, 120)
}

func compactSessionText(text string, max int) string {
	if max <= 0 {
		return ""
	}
	text = strings.TrimSpace(text)
	if text == "" {
		return ""
	}
	text = strings.NewReplacer("\r", " ", "\n", " ").Replace(text)
	text = strings.Join(strings.Fields(text), " ")
	if len(text) <= max {
		return text
	}
	if max <= 1 {
		return text[:max]
	}
	return text[:max-1] + "…"
}
