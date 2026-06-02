package core

import (
	"time"

	"github.com/nijaru/ion/session"
)

// progressMode tracks the current turn lifecycle phase.
type ProgressMode int

const (
	StateReady      ProgressMode = iota
	StateIonizing
	StateStreaming
	StateWorking
	StateComplete
	StateCancelled
	StateBlocked
	StateError
)

// TurnSummary records metrics from the most recent completed turn.
type TurnSummary struct {
	Elapsed time.Duration
	Input   int
	Output  int
	Cost    float64
}

// SubagentProgress tracks the ephemeral state of a background worker.
type SubagentProgress struct {
	ID        string
	Name      string
	Intent    string
	Status    string
	Output    string
	Reasoning string
}

// InFlightState holds data for the currently active turn or streaming response.
type InFlightState struct {
	Pending                 *session.Entry
	PendingTools            map[string]*session.Entry
	Subagents               map[string]*SubagentProgress
	ReasonBuf               string
	StreamBuf               string
	StreamChunks            []string
	QueuedSteering          []string
	QueuedTurns             []string
	QueuedTurnsBackendOwned bool
	Thinking                bool
	Canceling               bool
	AgentCommitted          bool
	DrainUntilTurnStarted   bool
	DrainStartedAt          time.Time
}

// ProgressState holds turn-level metrics and overall progress status.
type ProgressState struct {
	Mode              ProgressMode
	LastError         string
	Status            string
	StatusUpdatedAt   time.Time
	LocalStatus       string
	LocalStatusAt     time.Time
	ReasoningEffort   string
	TurnStartedAt     time.Time
	CurrentTurnInput  int
	CurrentTurnOutput int
	CurrentTurnCost   float64
	BudgetStopReason  string
	Compacting        bool
	LastTurnSummary   TurnSummary
	TokensSent        int
	TokensReceived    int
	ContextTokens     int
	TotalCost         float64
	LastToolUseID     string
}
