// Package runtime provides the integration layer between the agent harness
// and the TUI/CLI. It owns the agent lifecycle, model management, session
// switching, and compaction.
//
// Pi equivalent: pi-coding-agent/core/agent-session.js (2,534 lines)
//
// AgentSession is the bridge between the headless agent and any UI.
// Both TUI and headless modes use it, each adding their own I/O.
package runtime

import (
	"context"
	"fmt"
	"sync"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// AgentSession owns the agent lifecycle and provides the API for all run modes.
//
// Design: Pi's AgentSession is shared between interactive, print, and rpc modes.
// Each mode adds its own I/O layer on top.
type AgentSession struct {
	// Core handles
	runner  agent.Runner
	session session.Session
	store   session.Store
	config  *config.Config

	// Model management
	model     llm.Model
	thinking  session.ThinkingLevel
	activePreset Preset

	// Runtime state
	inFlight InFlightState
	progress ProgressState

	// Event subscription
	mu        sync.RWMutex
	listeners []func(session.Event)

	// Compaction
	compaction    agent.CompactionSettings
	contextWindow int
}

// NewAgentSession creates a new AgentSession.
func NewAgentSession(cfg AgentSessionConfig) *AgentSession {
	return &AgentSession{
		runner:        cfg.Runner,
		session:       cfg.Session,
		store:         cfg.Store,
		config:        cfg.Config,
		model:         cfg.Model,
		thinking:      cfg.Thinking,
		activePreset:  cfg.ActivePreset,
		compaction:    cfg.Compaction,
		contextWindow: cfg.ContextWindow,
	}
}

// AgentSessionConfig holds the configuration for creating an AgentSession.
type AgentSessionConfig struct {
	Runner        agent.Runner
	Session       session.Session
	Store         session.Store
	Config        *config.Config
	Model         llm.Model
	Thinking      session.ThinkingLevel
	ActivePreset  Preset
	Compaction    agent.CompactionSettings
	ContextWindow int
}

// --- Core API (used by all run modes) ---

// Submit submits a user message and runs a full agent turn.
// Blocks until the turn completes.
func (a *AgentSession) Submit(ctx context.Context, text string) (session.Message, error) {
	a.inFlight.Canceling = false
	a.progress.Mode = StateStreaming
	defer func() {
		a.inFlight.Canceling = false
		a.progress.Mode = StateReady
	}()

	msg, err := a.runner.Prompt(ctx, text)
	if err != nil {
		return nil, err
	}

	// Auto-compaction check
	if agent.ShouldCompactAfterTurn(ctx, a.session, a.contextWindow, a.compaction) {
		if compactErr := a.Compact(ctx); compactErr != nil {
			// Log but don't fail the turn
			a.emit(&session.Error{Err: fmt.Errorf("auto-compact: %w", compactErr)})
		}
	}

	return msg, nil
}

// Steer queues a steering message (mid-turn direction change).
func (a *AgentSession) Steer(text string) {
	a.runner.Steer(text)
}

// FollowUp queues a follow-up message for the next turn.
func (a *AgentSession) FollowUp(text string) {
	a.runner.FollowUp(text)
}

// NextTurn queues a message to start a new turn.
func (a *AgentSession) NextTurn(text string) {
	a.runner.NextTurn(text)
}

// Abort cancels the current turn.
func (a *AgentSession) Abort() error {
	return a.runner.Abort()
}

// Events returns the channel of agent events.
func (a *AgentSession) Events() <-chan session.Event {
	return a.runner.Events()
}

// --- Model management ---

// SetModel switches the active model.
func (a *AgentSession) SetModel(model llm.Model) {
	a.model = model
	a.runner.SetModel(model)
}

// SetThinking changes the thinking level.
func (a *AgentSession) SetThinking(level session.ThinkingLevel) {
	a.thinking = level
	a.runner.SetThinking(level)
}

// Model returns the current model.
func (a *AgentSession) Model() llm.Model {
	return a.model
}

// Thinking returns the current thinking level.
func (a *AgentSession) Thinking() session.ThinkingLevel {
	return a.thinking
}

// ActivePreset returns the active preset.
func (a *AgentSession) ActivePreset() Preset {
	return a.activePreset
}

// SetActivePreset sets the active preset.
func (a *AgentSession) SetActivePreset(preset Preset) {
	a.activePreset = preset
}

// --- Session management ---

// Session returns the underlying session.
func (a *AgentSession) Session() session.Session {
	return a.session
}

// Store returns the underlying store.
func (a *AgentSession) Store() session.Store {
	return a.store
}

// Config returns the current config.
func (a *AgentSession) Config() *config.Config {
	return a.config
}

// SetConfig updates the config.
func (a *AgentSession) SetConfig(cfg *config.Config) {
	a.config = cfg
}

// SetSession replaces the current session (for session switching).
func (a *AgentSession) SetSession(sess session.Session) {
	a.session = sess
}

// SetRunner replaces the current runner (for model/session switching).
func (a *AgentSession) SetRunner(runner agent.Runner) {
	a.runner = runner
}

// --- Compaction ---

// Compact triggers context compaction.
func (a *AgentSession) Compact(ctx context.Context) error {
	a.progress.Mode = StateWorking
	a.progress.Status = "compacting"
	defer func() {
		a.progress.Mode = StateReady
		a.progress.Status = ""
	}()

	result, err := agent.Compact(ctx, a.session, nil, a.model.ID, a.compaction)
	if err != nil {
		return fmt.Errorf("compact: %w", err)
	}
	if result != nil {
		a.emit(&session.Error{Err: fmt.Errorf("compacted: %d tokens → summary", result.TokensBefore)})
	}
	return nil
}

// --- State access ---

// InFlight returns the current in-flight state.
func (a *AgentSession) InFlight() *InFlightState {
	return &a.inFlight
}

// Progress returns the current progress state.
func (a *AgentSession) Progress() *ProgressState {
	return &a.progress
}

// --- Event subscription ---

// Subscribe registers a listener for agent events.
func (a *AgentSession) Subscribe(listener func(session.Event)) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.listeners = append(a.listeners, listener)
}

// emit sends an event to all listeners.
func (a *AgentSession) emit(e session.Event) {
	a.mu.RLock()
	defer a.mu.RUnlock()
	for _, listener := range a.listeners {
		listener(e)
	}
}
