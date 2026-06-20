package agent

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Agent is the high-level agent session wrapper.
//
// It composes:
//   - AgentLoop (pure turn sequencing)
//   - Recovery (overflow/retry)
//   - Persistence (session store)
//   - Queue management (steering/follow-up)
//   - Lifecycle (Open/Resume/Close)
//
// The Agent struct is the public API. The AgentLoop is the internal implementation.
type Agent struct {
	config    AgentConfig
	state     AgentState
	listeners map[uint64]func(session.AgentEvent)
	nextID    uint64
	mu        sync.RWMutex
	tree      *session.TreeStore

	// Session state
	id            string
	events        chan session.AgentEvent
	closed        bool
	closeOnce     sync.Once
	steeringQueue []string
	followUpQueue []string
	nextTurnQueue []string
	turnCtx       context.Context
	cancel        context.CancelFunc
	idleCh        chan struct{} // closed when turn completes

	// Recovery state
	overflowAttempted bool
	retryAttempt      int
	contextTokens     int

	// Session persistence
	store session.SessionStore
	sess  session.SessionHandle
}

// New creates a new Agent with the given configuration.
func New(config AgentConfig) *Agent {
	id := config.ID
	if id == "" {
		id = "default"
	}
	a := &Agent{
		config: config,
		state: AgentState{
			Model:           config.Model,
			ThinkingLevel:   config.ThinkingLevel,
			SystemPrompt:    config.SystemPrompt,
			Tools:           config.Tools,
			PendingToolCalls: make(map[string]bool),
		},
		id:     id,
		events: make(chan session.AgentEvent, 100),
		tree:   session.NewTreeStore(),
	}
	// Wire OnEvent to send to events channel if not already set.
	if a.config.OnEvent == nil {
		a.config.OnEvent = func(ev session.AgentEvent) {
			defer func() { recover() }()
			select {
			case a.events <- ev:
			default:
				// Channel full — event dropped. Log for observability.
				// Blocking would stall the agent loop.
				if a.config.Logger != nil {
					a.config.Logger.Warn("event channel full, dropping event")
				}
			}
		}
	}
	// Wire OnModelMessage to appendModelMessage if not already set.
	if a.config.OnModelMessage == nil {
		a.config.OnModelMessage = a.appendModelMessage
	}
	// Wire queue callbacks if not already set.
	// Each queue uses its own mode (steering vs follow-up).
	defaultMode := a.config.QueueMode
	if defaultMode == "" {
		defaultMode = QueueModeOneAtATime
	}
	if a.config.GetSteeringMessages == nil {
		a.config.GetSteeringMessages = func() []AgentMessage {
			a.mu.Lock()
			defer a.mu.Unlock()
			if len(a.steeringQueue) == 0 {
				return nil
			}
			mode := a.config.SteeringMode
			if mode == "" {
				mode = defaultMode
			}
			msgs := drainQueuedMessagesLocked(&a.steeringQueue, mode)
			a.emitQueueUpdatedLocked()
			return msgs
		}
	}
	if a.config.GetFollowUpMessages == nil {
		a.config.GetFollowUpMessages = func() []AgentMessage {
			a.mu.Lock()
			defer a.mu.Unlock()
			if len(a.followUpQueue) == 0 {
				return nil
			}
			mode := a.config.FollowUpMode
			if mode == "" {
				mode = defaultMode
			}
			msgs := drainQueuedMessagesLocked(&a.followUpQueue, mode)
			a.emitQueueUpdatedLocked()
			return msgs
		}
	}
	return a
}

func (a *Agent) emit(ev session.AgentEvent) {
	a.mu.RLock()
	closed := a.closed
	onEvent := a.config.OnEvent
	listeners := make([]func(session.AgentEvent), 0, len(a.listeners))
	for _, l := range a.listeners {
		listeners = append(listeners, l)
	}
	a.mu.RUnlock()
	if closed {
		return
	}

	// Track streaming state (Pi parity)
	a.trackEventState(ev)

	if onEvent != nil {
		onEvent(ev)
	}
	for _, l := range listeners {
		l(ev)
	}
}

// trackEventState updates agent state based on events.
// Pi parity: track streamingMessage and pendingToolCalls.
func (a *Agent) trackEventState(ev session.AgentEvent) {
	switch e := ev.(type) {
	case session.MessageStart:
		a.mu.Lock()
		msg := AgentMessage{
			Role:      "assistant",
			Timestamp: time.Now().UnixMilli(),
		}
		a.state.StreamingMessage = &msg
		a.mu.Unlock()
	case session.MessageUpdate:
		a.mu.Lock()
		if a.state.StreamingMessage != nil {
			a.state.StreamingMessage = &AgentMessage{
				Role:         "assistant",
				InputTokens:  e.Message.InputTokens,
				OutputTokens: e.Message.OutputTokens,
				TotalTokens:  e.Message.TotalTokens,
				Cost:         e.Message.Cost,
			}
		}
		a.mu.Unlock()
	case session.MessageEnd:
		a.mu.Lock()
		a.state.StreamingMessage = nil
		a.mu.Unlock()
	case session.ToolCallStart:
		a.mu.Lock()
		a.state.PendingToolCalls[e.ToolUseID] = true
		a.mu.Unlock()
	case session.ToolCallEnd:
		a.mu.Lock()
		delete(a.state.PendingToolCalls, e.ToolUseID)
		a.mu.Unlock()
	}
}

// emitLocked sends an event without acquiring the lock.
// Caller must hold a.mu.
func (a *Agent) emitLocked(ev session.AgentEvent) {
	if a.closed {
		return
	}
	onEvent := a.config.OnEvent
	if onEvent != nil {
		onEvent(ev)
	}
}

func (a *Agent) emitInputMessage(message AgentMessage) {
	if message.Role != "user" {
		return
	}
	a.emit(session.UserMessage{
		Base:    session.BaseNow(),
		Message: message.TextContent(),
	})
}

// toSessionAgentMessages converts domain AgentMessages to session AgentMessages
// for event payloads (TurnEnd, AgentEnd).
func toSessionAgentMessages(msgs []AgentMessage) []session.AgentMessage {
	if len(msgs) == 0 {
		return nil
	}
	sm := make([]session.AgentMessage, len(msgs))
	for i, m := range msgs {
		sm[i] = session.AgentMessage{
			Message:      m.TextContent(),
			Reasoning:    m.ReasoningContent(),
			InputTokens:  m.InputTokens,
			OutputTokens: m.OutputTokens,
			TotalTokens:  m.TotalTokens,
			Cost:         m.Cost,
		}
	}
	return sm
}

// State returns a copy of the current agent state.
func (a *Agent) State() AgentState {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.state
}

// IsTurnInProgress returns true if a turn is currently being executed.
func (a *Agent) IsTurnInProgress() bool {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.turnCtx != nil
}

// SetSystemPrompt sets the system prompt for the agent.
func (a *Agent) SetSystemPrompt(prompt string) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.state.SystemPrompt = prompt
}

// SetTools sets the available tools for the agent.
// This updates both AllTools (full list) and Tools (active list).
func (a *Agent) SetTools(tools []AgentTool) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.state.AllTools = tools
	a.state.Tools = tools
}

// SetSteeringMode sets the steering queue mode.
func (a *Agent) SetSteeringMode(mode QueueMode) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.config.SteeringMode = mode
}

// SetFollowUpMode sets the follow-up queue mode.
func (a *Agent) SetFollowUpMode(mode QueueMode) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.config.FollowUpMode = mode
}

// AppendMessage appends a message to the conversation history.
func (a *Agent) AppendMessage(msg AgentMessage) {
	a.mu.Lock()
	defer a.mu.Unlock()

	// Add to tree store
	var parentID *string
	if leaf := a.tree.Leaf(); leaf != nil {
		id := leaf.ID
		parentID = &id
	}
	llmMsg := agentMessageToLLM(msg)
	entryID := a.tree.NextID()
	entry := session.NewMessageEntry(entryID, parentID, llmMsg)
	if err := a.tree.Add(entry); err == nil {
		a.tree.SetLeaf(entryID)
	}
}

// NextTurn queues a message for the next turn.
// The message will be injected when the user sends a new message.
func (a *Agent) NextTurn(msg AgentMessage) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.nextTurnQueue = append(a.nextTurnQueue, msg.TextContent())
	a.emitQueueUpdatedLocked()
}

// SetActiveTools sets the active tool names.
// Only tools with these names will be available for the next turn.
// The full tool list (AllTools) is preserved so tools can be re-enabled.
func (a *Agent) SetActiveTools(toolNames []string) {
	a.mu.Lock()
	defer a.mu.Unlock()

	// Build name set
	nameSet := make(map[string]bool, len(toolNames))
	for _, name := range toolNames {
		nameSet[name] = true
	}

	// Filter from AllTools (not Tools) to preserve the full list
	active := make([]AgentTool, 0, len(toolNames))
	for _, tool := range a.state.AllTools {
		if nameSet[tool.Name] {
			active = append(active, tool)
		}
	}
	a.state.Tools = active

	// Add active tools change entry to tree
	var parentID *string
	if leaf := a.tree.Leaf(); leaf != nil {
		id := leaf.ID
		parentID = &id
	}
	entryID := a.tree.NextID()
	entry := session.NewActiveToolsChangeEntry(entryID, parentID, toolNames)
	if err := a.tree.Add(entry); err == nil {
		a.tree.SetLeaf(entryID)
	}
}

// SetModel sets the model for the agent.
func (a *Agent) SetModel(model llm.Model) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.state.Model = model
	a.config.Model = model

	// Add model change entry to tree
	var parentID *string
	if leaf := a.tree.Leaf(); leaf != nil {
		id := leaf.ID
		parentID = &id
	}
	entryID := a.tree.NextID()
	entry := session.NewModelChangeEntry(entryID, parentID, model.Provider, model.ID)
	if err := a.tree.Add(entry); err == nil {
		a.tree.SetLeaf(entryID)
	}
}

// SetThinkingLevel sets the thinking level for the agent.
func (a *Agent) SetThinkingLevel(level ThinkingLevel) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.state.ThinkingLevel = level
	a.config.ThinkingLevel = level

	// Add thinking level change entry to tree
	var parentID *string
	if leaf := a.tree.Leaf(); leaf != nil {
		id := leaf.ID
		parentID = &id
	}
	entryID := a.tree.NextID()
	entry := session.NewThinkingLevelChangeEntry(entryID, parentID, string(level))
	if err := a.tree.Add(entry); err == nil {
		a.tree.SetLeaf(entryID)
	}
}

// SetMessages replaces the provider-visible conversation history.
func (a *Agent) SetMessages(messages []AgentMessage) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.setMessagesLocked(messages)
}

// GetResources returns the current resources (skills and prompt templates).
func (a *Agent) GetResources() AgentResources {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.config.Resources
}

// SetResources sets the resources (skills and prompt templates).
// Emits a ResourcesUpdate event.
func (a *Agent) SetResources(resources AgentResources) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.config.Resources = resources

	// Emit resources_update event
	skills := make([]session.AgentSkill, len(resources.Skills))
	for i, s := range resources.Skills {
		skills[i] = session.AgentSkill{
			Name:        s.Name,
			Description: s.Description,
			Location:    s.Location,
		}
	}
	templates := make([]session.AgentPromptTemplate, len(resources.PromptTemplates))
	for i, t := range resources.PromptTemplates {
		templates[i] = session.AgentPromptTemplate{
			Name:        t.Name,
			Description: t.Description,
		}
	}
	a.emit(session.ResourcesUpdate{
		Skills:          skills,
		PromptTemplates: templates,
	})
}

// GetStreamOptions returns the current stream options.
// GetStreamOptions returns the current stream options.
func (a *Agent) GetStreamOptions() StreamOptions {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.config.StreamOptions
}

// SetTransformContext sets the TransformContext callback on the agent config.
// This allows the harness to wire up context hooks after agent creation.
func (a *Agent) SetTransformContext(fn func(ctx context.Context, messages []AgentMessage) []AgentMessage) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.config.TransformContext = fn
}

// Config returns a copy of the agent config.
func (a *Agent) Config() AgentConfig {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.config
}

// SetStreamOptions merges stream options.
// Zero-value fields are left unchanged (merged, not replaced).
func (a *Agent) SetStreamOptions(opts StreamOptions) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if opts.TimeoutMs != 0 {
		a.config.StreamOptions.TimeoutMs = opts.TimeoutMs
	}
	if opts.MaxRetries != 0 {
		a.config.StreamOptions.MaxRetries = opts.MaxRetries
	}
	if opts.MaxRetryDelayMs != 0 {
		a.config.StreamOptions.MaxRetryDelayMs = opts.MaxRetryDelayMs
	}
	if opts.Headers != nil {
		if a.config.StreamOptions.Headers == nil {
			a.config.StreamOptions.Headers = make(map[string]string)
		}
		for k, v := range opts.Headers {
			a.config.StreamOptions.Headers[k] = v
		}
	}
}

// Skill executes a turn with a skill invocation.
// Finds the named skill in resources, formats it as a skill block, and runs a turn.
// additionalInstructions is appended after the skill block.
func (a *Agent) Skill(ctx context.Context, name string, additionalInstructions string) ([]AgentMessage, error) {
	a.mu.RLock()
	resources := a.config.Resources
	a.mu.RUnlock()

	var found *Skill
	for i := range resources.Skills {
		if resources.Skills[i].Name == name {
			found = &resources.Skills[i]
			break
		}
	}
	if found == nil {
		return nil, fmt.Errorf("unknown skill: %s", name)
	}

	prompt := formatSkillInvocation(*found, additionalInstructions)
	return a.Run(ctx, []AgentMessage{{
		Role:  "user",
		Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: prompt}},
	}})
}

// PromptFromTemplate executes a turn with a prompt template.
// Finds the named template in resources, substitutes args, and runs a turn.
func (a *Agent) PromptFromTemplate(ctx context.Context, name string, args []string) ([]AgentMessage, error) {
	a.mu.RLock()
	resources := a.config.Resources
	a.mu.RUnlock()

	var found *PromptTemplate
	for i := range resources.PromptTemplates {
		if resources.PromptTemplates[i].Name == name {
			found = &resources.PromptTemplates[i]
			break
		}
	}
	if found == nil {
		return nil, fmt.Errorf("unknown prompt template: %s", name)
	}

	prompt := formatPromptTemplateInvocation(*found, args)
	return a.Run(ctx, []AgentMessage{{
		Role:  "user",
		Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: prompt}},
	}})
}

// formatSkillInvocation formats a skill as an invocation prompt.
func formatSkillInvocation(skill Skill, additionalInstructions string) string {
	var b strings.Builder
	b.WriteString(fmt.Sprintf("<skill name=\"%s\" location=\"%s\">\n", skill.Name, skill.Location))
	b.WriteString(fmt.Sprintf("References are relative to %s.\n\n", filepath.Dir(skill.Location)))
	b.WriteString(skill.Content)
	b.WriteString("\n</skill>")
	if additionalInstructions != "" {
		b.WriteString("\n\n")
		b.WriteString(additionalInstructions)
	}
	return b.String()
}

// formatPromptTemplateInvocation substitutes args into a template.
// Supports $1, $2, ..., $ARGUMENTS, $@ placeholders.
func formatPromptTemplateInvocation(template PromptTemplate, args []string) string {
	content := template.Content
	// Replace $N with indexed args (1-based)
	for i, arg := range args {
		content = strings.ReplaceAll(content, fmt.Sprintf("$%d", i+1), arg)
	}
	// Replace $ARGUMENTS and $@ with all args joined
	allArgs := strings.Join(args, " ")
	content = strings.ReplaceAll(content, "$ARGUMENTS", allArgs)
	content = strings.ReplaceAll(content, "$@", allArgs)
	return content
}

// Signal returns the active abort signal for the current run, if any.
// Returns nil if no run is active.
func (a *Agent) Signal() context.Context {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.turnCtx
}

// Abort aborts the current run, if one is active.
func (a *Agent) Abort() {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.cancel != nil {
		a.cancel()
	}
}

// HasQueuedMessages returns true if either queue contains pending messages.
func (a *Agent) HasQueuedMessages() bool {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return len(a.steeringQueue) > 0 || len(a.followUpQueue) > 0
}

// ClearSteeringQueue removes all queued steering messages.
func (a *Agent) ClearSteeringQueue() {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.steeringQueue = nil
	a.emitQueueUpdatedLocked()
}

// ClearFollowUpQueue removes all queued follow-up messages.
func (a *Agent) ClearFollowUpQueue() {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.followUpQueue = nil
	a.emitQueueUpdatedLocked()
}

// SteeringQueue returns a copy of the current steering queue.
func (a *Agent) SteeringQueue() []string {
	a.mu.RLock()
	defer a.mu.RUnlock()
	result := make([]string, len(a.steeringQueue))
	copy(result, a.steeringQueue)
	return result
}

// FollowUpQueue returns a copy of the current follow-up queue.
func (a *Agent) FollowUpQueue() []string {
	a.mu.RLock()
	defer a.mu.RUnlock()
	result := make([]string, len(a.followUpQueue))
	copy(result, a.followUpQueue)
	return result
}

// ClearAllQueues removes all queued steering and follow-up messages.
func (a *Agent) ClearAllQueues() {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.steeringQueue = nil
	a.followUpQueue = nil
	a.nextTurnQueue = nil
	a.emitQueueUpdatedLocked()
}

// setMessagesLocked replaces messages in the tree store.
// Caller must hold a.mu.
func (a *Agent) setMessagesLocked(messages []AgentMessage) {
	a.tree = session.NewTreeStore()
	var parentID *string
	for _, msg := range messages {
		llmMsg := agentMessageToLLM(msg)
		entryID := a.tree.NextID()
		entry := session.NewMessageEntry(entryID, parentID, llmMsg)
		if err := a.tree.Add(entry); err == nil {
			a.tree.SetLeaf(entryID)
			id := entryID
			parentID = &id
		}
	}
}

// newLoop creates a new AgentLoop with the current agent state.
// Caller must hold a.mu (read lock is sufficient).
func (a *Agent) newLoop() *AgentLoop {
	loop := NewAgentLoop(a.config, a.state, a.emit, a.id)
	loop.tree = a.tree
	return loop
}

// syncLoopState copies the loop state back to the agent state.
// Caller must hold a.mu.
func (a *Agent) syncLoopState(loop *AgentLoop) {
	loopState := loop.State()
	a.state.Model = loopState.Model
	a.state.ThinkingLevel = loopState.ThinkingLevel
	a.state.Tools = loopState.Tools
	a.state.SystemPrompt = loopState.SystemPrompt
}

// Run starts the agent loop with the given prompt messages.
// It returns the new messages added during the run.
// Emits AgentStart at the beginning. The loop emits AgentEnd.
// Emits TurnEnd per-turn inside the loop (Pi parity).
func (a *Agent) Run(ctx context.Context, prompts []AgentMessage) ([]AgentMessage, error) {
	a.mu.Lock()
	a.state.IsStreaming = true
	a.state.ErrorMessage = ""
	a.state.StreamingMessage = nil

	// Inject nextTurn messages before user prompts
	nextTurnMsgs := a.drainNextTurnLocked()

	loop := a.newLoop()
	a.mu.Unlock()

	defer func() {
		a.mu.Lock()
		a.state.IsStreaming = false
		a.state.StreamingMessage = nil
		a.mu.Unlock()
	}()

	// Combine nextTurn messages with user prompts
	allPrompts := append(nextTurnMsgs, prompts...)
	newMessages, err := loop.Run(ctx, allPrompts)

	a.mu.Lock()
	a.syncLoopState(loop)
	if err != nil {
		a.state.ErrorMessage = err.Error()
	}
	// Persist tree store
	if saveErr := a.saveTree(); saveErr != nil {
		// Log but don't fail — tree persistence is optional
		slog.Warn("failed to save tree", "error", saveErr)
	}
	a.mu.Unlock()

	return newMessages, err
}

// Continue continues the agent loop without adding new messages.
// Used for retries — context already has user message or tool results.
// Emits AgentStart at the beginning. The loop emits AgentEnd.
func (a *Agent) Continue(ctx context.Context) ([]AgentMessage, error) {
	a.mu.Lock()
	a.state.IsStreaming = true
	a.state.ErrorMessage = ""
	a.state.StreamingMessage = nil
	loop := a.newLoop()
	a.mu.Unlock()

	defer func() {
		a.mu.Lock()
		a.state.IsStreaming = false
		a.state.StreamingMessage = nil
		a.mu.Unlock()
	}()

	newMessages, err := loop.Continue(ctx)

	a.mu.Lock()
	a.syncLoopState(loop)
	if err != nil {
		a.state.ErrorMessage = err.Error()
	}
	// Persist tree store
	if saveErr := a.saveTree(); saveErr != nil {
		// Log but don't fail — tree persistence is optional
		slog.Warn("failed to save tree", "error", saveErr)
	}
	a.mu.Unlock()

	return newMessages, err
}

// Open initializes or creates a new session.
func (a *Agent) Open(ctx context.Context) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return fmt.Errorf("session is closed")
	}

	return nil
}

// Resume loads an existing session.
func (a *Agent) Resume(ctx context.Context, sessionID string) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return fmt.Errorf("session is closed")
	}

	a.id = sessionID

	// Try to load tree store from file
	treePath := a.treePath()
	if tree, err := session.LoadTreeStore(treePath); err == nil {
		a.tree = tree
	} else if history, err := a.loadModelHistoryLocked(ctx); err != nil {
		return err
	} else if history != nil {
		a.setMessagesLocked(history)
	}

	return nil
}

// treePath returns the path to the tree store file for this session.
func (a *Agent) treePath() string {
	return fmt.Sprintf(".ion/sessions/%s/tree.json", a.id)
}

// saveTree persists the tree store to disk.
func (a *Agent) saveTree() error {
	return a.tree.Save(a.treePath())
}

// CancelTurn interrupts an in-flight turn if the backend supports it.
func (a *Agent) CancelTurn(ctx context.Context) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return fmt.Errorf("session is closed")
	}

	if a.cancel != nil {
		a.cancel()
	}
	return nil
}

// Close terminates the session and cleans up resources.
func (a *Agent) Close() error {
	a.closeOnce.Do(func() {
		a.mu.Lock()
		a.closed = true
		if a.cancel != nil {
			a.cancel()
		}
		a.mu.Unlock()
		// Do not close a.events — emit guards with a.closed under lock.
		// Closing would race with concurrent emit calls.
	})
	return nil
}

// Events returns a read-only channel of typed events emitted by the session.
func (a *Agent) Events() <-chan session.AgentEvent {
	return a.events
}

// ID returns the session identifier.
func (a *Agent) ID() string {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.id
}

// LeafID returns the current leaf entry ID.
func (a *Agent) LeafID() string {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.tree.LeafID()
}

// Meta returns session metadata.
func (a *Agent) Meta() map[string]string {
	a.mu.Lock()
	defer a.mu.Unlock()

	return map[string]string{
		"backend": "agent",
		"model":   a.config.Model.ID,
	}
}

// SetStore sets the storage store.
func (a *Agent) SetStore(store session.SessionStore) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.store = store
}

// SetSession sets the storage session.
func (a *Agent) SetSession(sess session.SessionHandle) {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.sess = sess
}

// Session returns the storage session handle.
func (a *Agent) Session() session.SessionHandle {
	a.mu.RLock()
	defer a.mu.RUnlock()
	return a.sess
}

// SteerTurn sends steering input during an active turn.
func (a *Agent) SteerTurn(
	ctx context.Context,
	text string,
) (session.SteeringResult, error) {
	a.mu.Lock()
	if a.closed {
		a.mu.Unlock()
		return session.SteeringResult{}, fmt.Errorf("session is closed")
	}
	a.steeringQueue = append(a.steeringQueue, text)
	a.emitQueueUpdatedLocked()
	a.mu.Unlock()

	return session.SteeringResult{
		Outcome: session.SteeringAccepted,
		Notice:  "Steering input accepted",
	}, nil
}

// FollowUpTurn sends follow-up input after the agent would stop.
func (a *Agent) FollowUpTurn(
	ctx context.Context,
	text string,
) (session.QueuedInputResult, error) {
	a.mu.Lock()
	if a.closed {
		a.mu.Unlock()
		return session.QueuedInputResult{}, fmt.Errorf("session is closed")
	}
	a.followUpQueue = append(a.followUpQueue, text)
	a.emitQueueUpdatedLocked()
	a.mu.Unlock()

	return session.QueuedInputResult{
		Outcome: session.QueuedInputAccepted,
		Notice:  "Follow-up input accepted",
	}, nil
}

// ClearQueuedInput clears queued input and returns the snapshot.
func (a *Agent) ClearQueuedInput(
	ctx context.Context,
) (session.QueuedInputSnapshot, error) {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return session.QueuedInputSnapshot{}, fmt.Errorf("session is closed")
	}

	snapshot := session.QueuedInputSnapshot{
		Steering: append([]string(nil), a.steeringQueue...),
		FollowUp: append([]string(nil), a.followUpQueue...),
	}

	a.steeringQueue = nil
	a.followUpQueue = nil
	a.nextTurnQueue = nil
	a.emitQueueUpdatedLocked()

	return snapshot, nil
}

func (a *Agent) emitQueueUpdatedLocked() {
	snapshot := session.QueuedInputSnapshot{
		Steering: append([]string(nil), a.steeringQueue...),
		FollowUp: append([]string(nil), a.followUpQueue...),
		NextTurn: append([]string(nil), a.nextTurnQueue...),
	}
	a.emitLocked(session.QueuedInputUpdate{
		Base:     session.BaseNow(),
		Snapshot: snapshot,
	})
}

// handlePostAgentRun handles post-agent-run logic including overflow
// recovery and auto-retry with exponential backoff.
// The loop emits AgentEnd (single ownership). This wrapper handles recovery.
func (a *Agent) handlePostAgentRun(ctx context.Context, err error, newMessages []AgentMessage) {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return
	}

	// Success or cancellation — loop already emitted AgentEnd
	if err == nil || errors.Is(err, context.Canceled) {
		a.retryAttempt = 0
		return
	}

	errMsg := err.Error()
	agentErr := NewAgentError(errMsg, err)

	// Overflow recovery: compact and retry once
	if agentErr.Code == ErrCodeOverflow && !a.overflowAttempted {
		a.overflowAttempted = true
		if a.recoverFromOverflow(ctx) {
			return
		}
	}

	// Transient error retry with exponential backoff
	if agentErr.IsRetryable && a.retryAttempt < a.config.GetMaxRetries() {
		if a.retryWithBackoff(ctx, errMsg) {
			return
		}
	}

	// Non-retryable error — loop already emitted AgentEnd
	// Call HandleRunFailure if configured
	if a.config.HandleRunFailure != nil {
		a.config.HandleRunFailure(err)
	}
}

// recoverFromOverflow handles context overflow by compacting and retrying.
// Returns true if recovery succeeded or was attempted. Caller must hold a.mu.
func (a *Agent) recoverFromOverflow(ctx context.Context) bool {
	a.emitLocked(session.CompactionTrigger{
		Base:   session.BaseNow(),
		Reason: "overflow",
	})
	a.trimLastAssistantMessage()

	// Unlock for blocking compaction call
	a.mu.Unlock()
	defer a.mu.Lock()

	compacted, err := a.runCompaction(ctx)
	if err != nil {
		if !a.closed {
			a.emitLocked(session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				FinalError: fmt.Sprintf("compaction failed: %v", err),
			})
		}
		return true
	}
	if compacted {
		a.resetContextTokens()
	}

	// Retry the turn. The loop will emit AgentEnd.
	if _, err := a.Continue(ctx); err != nil {
		a.emitLocked(session.AutoRetryEnd{
			Base:       session.BaseNow(),
			Success:    false,
			FinalError: fmt.Sprintf("retry after compaction failed: %v", err),
		})
	}
	return true
}

// retryWithBackoff retries a failed turn with exponential backoff.
// Returns true if retry was attempted. Caller must hold a.mu.
func (a *Agent) retryWithBackoff(ctx context.Context, errMsg string) bool {
	a.retryAttempt++
	delayMs := a.config.GetRetryBaseDelayMs() * (1 << (a.retryAttempt - 1))

	a.emitLocked(session.AutoRetryStart{
		Base:       session.BaseNow(),
		Attempt:    a.retryAttempt,
		MaxAttempt: a.config.GetMaxRetries(),
		DelayMs:    delayMs,
		Error:      errMsg,
	})
	a.trimLastAssistantMessage()

	// Unlock for blocking delay
	a.mu.Unlock()
	select {
	case <-ctx.Done():
		a.mu.Lock()
		if !a.closed {
			a.emitLocked(session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				Attempt:    a.retryAttempt,
				FinalError: "Retry cancelled",
			})
		}
		return true
	case <-time.After(time.Duration(delayMs) * time.Millisecond):
	}
	a.mu.Lock()

	if a.closed {
		return true
	}

	// Retry the turn (unlock for blocking call)
	a.mu.Unlock()
	if _, retryErr := a.Continue(ctx); retryErr != nil {
		a.mu.Lock()
		if !a.closed {
			a.emitLocked(session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				Attempt:    a.retryAttempt,
				FinalError: fmt.Sprintf("retry failed: %v", retryErr),
			})
		}
		a.retryAttempt = 0
		return true
	}
	a.mu.Lock()

	if a.closed {
		return true
	}

	// Retry succeeded — loop already emitted AgentEnd
	a.emitLocked(session.AutoRetryEnd{
		Base:    session.BaseNow(),
		Success: true,
		Attempt: a.retryAttempt,
	})
	a.retryAttempt = 0
	return true
}

// writeModelMessage persists a message through the config callback.
func (a *Agent) writeModelMessage(ctx context.Context, message llm.Message) error {
	if a.config.OnModelMessage == nil {
		return nil
	}
	if isEmptyModelMessage(message) {
		return nil
	}
	return a.config.OnModelMessage(ctx, message)
}

func (a *Agent) appendModelMessage(ctx context.Context, message llm.Message) error {
	a.mu.Lock()
	sess := a.sess
	a.mu.Unlock()
	if sess == nil {
		return nil
	}
	return sess.AppendModelMessage(ctx, message)
}

func (a *Agent) loadModelHistoryLocked(ctx context.Context) ([]AgentMessage, error) {
	if a.sess == nil {
		return nil, nil
	}
	messages, err := a.sess.ModelMessages(ctx)
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
// Caller must hold a.mu.
func (a *Agent) trimLastAssistantMessage() {
	// This is handled by the loop's tree store now
}

// updateContextTokens updates the estimated context token count.
// Called from TokenUsage events.
// Caller must hold a.mu.
func (a *Agent) updateContextTokens(input, output int) {
	a.contextTokens += input + output
}

// needsCompaction checks if context tokens exceed the threshold.
// Returns true if compaction should be triggered.
// Caller must hold a.mu.
func (a *Agent) needsCompaction() bool {
	if a.config.Model.ContextWindow <= 0 {
		return false
	}
	// Use 80% threshold (matching Pi's default)
	threshold := int(float64(a.config.Model.ContextWindow) * 0.8)
	return a.contextTokens > threshold
}

// resetContextTokens resets the context token counter.
// Called after successful compaction.
// Caller must hold a.mu.
func (a *Agent) resetContextTokens() {
	a.contextTokens = 0
}

// Compact runs compaction on the session.
// Returns true if compaction occurred.
func (a *Agent) Compact(ctx context.Context) (bool, error) {
	return a.runCompaction(ctx)
}

// SetCompactionSummary sets a custom compaction summary from a hook.
func (a *Agent) SetCompactionSummary(summary string) {
	a.mu.Lock()
	defer a.mu.Unlock()
	// Add the summary as a compaction entry to the tree store
	var parentID *string
	if leaf := a.tree.Leaf(); leaf != nil {
		id := leaf.ID
		parentID = &id
	}
	a.tree.Add(session.NewCompactionEntry("hook-compaction", parentID, summary, "", 0))
}

// NavigateTreeOptions holds options for NavigateTree.
type NavigateTreeOptions struct {
	Summarize         bool
	CustomInstructions string
	Label             string
}

// NavigateTreeResult holds the result of NavigateTree.
type NavigateTreeResult struct {
	Cancelled    bool
	EditorText   string
	SummaryEntry *session.TreeEntry
}

// NavigateTree moves the active leaf to the target entry.
// If summarize is true and entries exist between old leaf and target,
// a branch summary is generated.
func (a *Agent) NavigateTree(ctx context.Context, targetID string, options NavigateTreeOptions) (NavigateTreeResult, error) {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.closed {
		return NavigateTreeResult{}, fmt.Errorf("session is closed")
	}

	oldLeafID := a.tree.LeafID()
	if oldLeafID == targetID {
		return NavigateTreeResult{Cancelled: false}, nil
	}

	// Get target entry
	target, ok := a.tree.Get(targetID)
	if !ok {
		return NavigateTreeResult{}, fmt.Errorf("entry %s not found", targetID)
	}

	// Collect entries between old leaf and target for branch summary
	commonAncestor := a.tree.CommonAncestor(oldLeafID, targetID)
	var entriesToSummarize []session.TreeEntry
	if oldLeafID != "" && commonAncestor != "" {
		// Collect entries from old leaf to common ancestor
		current := oldLeafID
		for current != commonAncestor {
			if entry, ok := a.tree.Get(current); ok {
				entriesToSummarize = append(entriesToSummarize, *entry)
			}
			if entry, ok := a.tree.Get(current); ok && entry.ParentID != nil {
				current = *entry.ParentID
			} else {
				break
			}
		}
	}

	// Fire session_before_tree hook
	if a.config.OnBeforeTreeNavigation != nil {
		result := a.config.OnBeforeTreeNavigation(ctx, targetID, oldLeafID, commonAncestor, entriesToSummarize, options.CustomInstructions)
		if result.Cancel {
			return NavigateTreeResult{Cancelled: true}, nil
			}
	}

	// Determine newLeafId and editorText
	var editorText string
	newLeafID := targetID
	if target.Type == session.EntryMessage && target.Message != nil && target.Message.Role == "user" {
		if target.ParentID != nil {
			newLeafID = *target.ParentID
		}
		editorText = target.Message.Content
	} else if target.Type == session.EntryCustom {
		// Custom entries: move to parent
		if target.ParentID != nil {
			newLeafID = *target.ParentID
		}
	}

	// Generate summary if requested and entries exist
	var summaryEntry *session.TreeEntry
	if options.Summarize && len(entriesToSummarize) > 0 {
		// Use LLM to generate summary
		if a.config.StreamFn != nil {
			smmary, err := a.generateBranchSummary(ctx, entriesToSummarize, options.CustomInstructions)
			if err == nil && smmary != "" {
				// Create branch_summary entry
				branchID := a.tree.NextID()
				branchEntry := session.NewBranchSummaryEntry(branchID, &newLeafID, oldLeafID, smmary)
				if err := a.tree.Add(branchEntry); err == nil {
					summaryEntry = branchEntry
				}
			}
		}
	}

	// Move leaf to target
	if newLeafID == "" {
		// If newLeafID is empty (target was root user message), use targetID
		newLeafID = targetID
	}
	if err := a.tree.SetLeaf(newLeafID); err != nil {
		return NavigateTreeResult{}, fmt.Errorf("set leaf: %w", err)
	}

	// Fire session_tree hook
	if a.config.OnAfterTreeNavigation != nil {
		a.config.OnAfterTreeNavigation(ctx, newLeafID, oldLeafID, summaryEntry)
	}

	return NavigateTreeResult{
		Cancelled:    false,
		EditorText:   editorText,
		SummaryEntry: summaryEntry,
	}, nil
}

// generateBranchSummary generates a summary of the branch using the LLM.
func (a *Agent) generateBranchSummary(ctx context.Context, entries []session.TreeEntry, customInstructions string) (string, error) {
	// Build conversation text from entries
	var conversationText string
	for _, entry := range entries {
		if entry.Message != nil {
			conversationText += string(entry.Message.Role) + ": " + entry.Message.Content + "\n\n"
		}
	}

	if conversationText == "" {
		return "No content to summarize", nil
	}

	// Build prompt
	prompt := "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n"
	if customInstructions != "" {
		prompt += "Additional focus: " + customInstructions + "\n\n"
	}
	prompt += "<conversation>\n" + conversationText + "</conversation>\n\n"
	prompt += "Create a structured summary of this conversation branch for context when returning later."

	// Call LLM
	messages := []llm.Message{
		{Role: "user", Content: prompt},
	}
	stream, err := a.config.StreamFn(ctx, &llm.Request{
		Model:    a.state.Model.ID,
		Messages: messages,
		MaxTokens: 2048,
	})
	if err != nil {
		return "", fmt.Errorf("generate summary: %w", err)
	}
	defer stream.Close()

	// Collect response
	var summary string
	for {
		chunk, ok := stream.Next()
		if !ok {
			break
		}
		if chunk.Content != "" {
			summary += chunk.Content
		}
	}

	if err := stream.Err(); err != nil {
		return "", fmt.Errorf("stream error: %w", err)
	}

	if summary == "" {
		return "No summary generated", nil
	}

	return summary, nil
}

// runCompaction runs the compaction function if available.
// Caller must NOT hold a.mu (blocking call).
func (a *Agent) runCompaction(ctx context.Context) (bool, error) {
	a.mu.Lock()
	compactFn := a.config.CompactFunc
	closed := a.closed
	a.mu.Unlock()

	if closed {
		return false, fmt.Errorf("session is closed")
	}
	if compactFn == nil {
		return false, nil
	}
	return compactFn(ctx)
}

// SubmitTurn sends a new user turn to the active session.
func (a *Agent) SubmitTurn(ctx context.Context, input string) error {
	a.mu.Lock()
	if a.closed {
		a.mu.Unlock()
		return fmt.Errorf("session is closed")
	}
	// Cancel any active running context first
	if a.cancel != nil {
		a.cancel()
	}
	turnCtx, cancel := context.WithCancel(ctx)
	a.turnCtx = turnCtx
	a.cancel = cancel
	a.idleCh = make(chan struct{})

	// Check if auto-compaction is needed before submitting
	if a.needsCompaction() && a.config.CompactFunc != nil {
		a.emitLocked(session.CompactionTrigger{
			Base:   session.BaseNow(),
			Reason: "threshold",
		})
		a.mu.Unlock()
		compacted, err := a.config.CompactFunc(ctx)
		a.mu.Lock()
		if err != nil {
			// Log compaction error but continue with the turn
			a.emitLocked(session.AutoRetryEnd{
				Base:       session.BaseNow(),
				Success:    false,
				FinalError: fmt.Sprintf("compaction failed: %v", err),
			})
		} else if compacted {
			a.resetContextTokens()
		}
	}

	// Create user message
	userMsg := AgentMessage{
		Role:  "user",
		Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: input}},
	}

	// Commit the user message to tree store synchronously.
	llmMsg := agentMessageToLLM(userMsg)
	var parentID *string
	if leaf := a.tree.Leaf(); leaf != nil {
		id := leaf.ID
		parentID = &id
	}
	entryID := a.tree.NextID()
	entry := session.NewMessageEntry(entryID, parentID, llmMsg)
	if err := a.tree.Add(entry); err == nil {
		a.tree.SetLeaf(entryID)
	}
	a.emitLocked(session.UserMessage{
		Base:    session.BaseNow(),
		Message: userMsg.TextContent(),
	})
	a.mu.Unlock()

	// Persist the user message (must happen outside lock to avoid deadlock
	// with appendModelMessage which acquires the lock).
	if err := a.writeModelMessage(turnCtx, agentMessageToLLM(userMsg)); err != nil {
		a.mu.Lock()
		a.cancel = nil
		if a.idleCh != nil {
			close(a.idleCh)
			a.idleCh = nil
		}
		a.turnCtx = nil
		a.mu.Unlock()
		cancel()
		return fmt.Errorf("write user message: %w", err)
	}

	// Run the agent loop in a goroutine
	go func() {
		defer func() {
			a.mu.Lock()
			// Only clear if we still own the turn context.
			// A newer SubmitTurn may have replaced it.
			if a.turnCtx == turnCtx {
				a.cancel = nil
				if a.idleCh != nil {
					close(a.idleCh)
					a.idleCh = nil
				}
				a.turnCtx = nil
				a.overflowAttempted = false
				a.retryAttempt = 0
			}
			a.mu.Unlock()
		}()
		newMessages, err := a.Continue(turnCtx)
		a.handlePostAgentRun(turnCtx, err, newMessages)
	}()

	return nil
}

// WaitForIdle blocks until the agent is idle (no active turn).
func (a *Agent) WaitForIdle(ctx context.Context) error {
	a.mu.RLock()
	if a.turnCtx == nil {
		a.mu.RUnlock()
		return nil
	}
	idleCh := a.idleCh
	a.mu.RUnlock()

	select {
	case <-idleCh:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// Reset clears the agent state and emits a fresh start.
func (a *Agent) Reset() {
	a.mu.Lock()
	defer a.mu.Unlock()

	// Cancel any active turn
	if a.cancel != nil {
		a.cancel()
	}

	// Clear state
	a.state.IsStreaming = false
	a.state.StreamingMessage = nil
	a.state.PendingToolCalls = make(map[string]bool)
	a.state.ErrorMessage = ""
	a.overflowAttempted = false
	a.retryAttempt = 0
	a.contextTokens = 0

	// Clear tree store
	a.tree = session.NewTreeStore()

	// Clear queues
	a.steeringQueue = nil
	a.followUpQueue = nil
	a.nextTurnQueue = nil

	// Emit fresh start
	a.emitLocked(session.AgentStart{Base: session.BaseNow()})
}

// Subscribe registers a listener for agent events.
// Returns an unsubscribe function.
func (a *Agent) Subscribe(listener func(session.AgentEvent)) func() {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.listeners == nil {
		a.listeners = make(map[uint64]func(session.AgentEvent))
	}
	a.nextID++
	id := a.nextID
	a.listeners[id] = listener

	return func() {
		a.mu.Lock()
		defer a.mu.Unlock()
		delete(a.listeners, id)
	}
}

// UpdateConfig updates the agent configuration.
func (a *Agent) UpdateConfig(config AgentConfig) {
	a.mu.Lock()
	defer a.mu.Unlock()

	a.config = config
	a.state.Model = config.Model
	a.state.ThinkingLevel = config.ThinkingLevel
	a.state.Tools = config.Tools
	a.state.SystemPrompt = config.SystemPrompt
}

// drainQueuedMessagesLocked drains messages from a queue based on the queue mode.
// Caller must hold a.mu.
func drainQueuedMessagesLocked(queue *[]string, mode QueueMode) []AgentMessage {
	if len(*queue) == 0 {
		return nil
	}
	count := 1
	if mode == QueueModeAll {
		count = len(*queue)
	}
	msgs := make([]AgentMessage, count)
	for i, text := range (*queue)[:count] {
		msgs[i] = AgentMessage{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: text}}}
	}
	*queue = (*queue)[count:]
	return msgs
}

// drainNextTurnLocked drains all nextTurn messages.
// nextTurn messages are always drained completely when a new turn starts.
func (a *Agent) drainNextTurnLocked() []AgentMessage {
	if len(a.nextTurnQueue) == 0 {
		return nil
	}
	msgs := make([]AgentMessage, len(a.nextTurnQueue))
	for i, text := range a.nextTurnQueue {
		msgs[i] = AgentMessage{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: text}}}
	}
	a.nextTurnQueue = nil
	a.emitQueueUpdatedLocked()
	return msgs
}
