package agent

import (
	"context"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// Command is the sealed interface for all controller operations. Every public
// runtime operation enters through a typed Command. The controller's handle
// function switches on the concrete type, and the exhaustive linter enforces
// that every command variant has a handler.
//
// Commands carry their own reply channel when a result is needed. The
// controller writes exactly one result before closing the channel. Callers
// that don't need a result use fire-and-forget commands without a reply
// channel.
type Command interface {
	command()
}

// --- Turn commands ---

// PromptCmd submits a user message and runs a full agent turn. The reply
// receives the final assistant message or a typed error.
type PromptCmd struct {
	Ctx    context.Context
	Text   string
	Images []session.ImageContent
	Reply  chan<- PromptResult
}

func (PromptCmd) command() {}

type PromptResult struct {
	Message session.Message
	Err     *TurnError
}

// turnCompletion is the only result path from a prompt worker back to the
// controller. The worker performs provider/session I/O, then waits for the
// controller to finalize lifecycle state and acknowledge the completion.
type turnCompletion struct {
	runDone chan struct{}
	reply   chan<- PromptResult
	message session.Message
	runErr  error
	ack     chan struct{}
}

// SteerCmd queues a steering message injected before the next assistant
// response. Returns an error if the runtime is idle or closed.
type SteerCmd struct {
	Text   string
	Images []session.ImageContent
	Reply  chan<- error
}

func (SteerCmd) command() {}

// FollowUpCmd queues a follow-up message for after the agent would stop.
type FollowUpCmd struct {
	Text   string
	Images []session.ImageContent
	Reply  chan<- error
}

func (FollowUpCmd) command() {}

// NextTurnCmd queues a message to start a new turn after settlement.
type NextTurnCmd struct {
	Text   string
	Images []session.ImageContent
	Reply  chan<- error
}

func (NextTurnCmd) command() {}

// AbortCmd cancels the current turn and clears queues. Returns the cleared
// steer and follow-up messages.
type AbortCmd struct {
	Reply chan<- AbortResult
}

func (AbortCmd) command() {}

type AbortResult struct {
	Steer    []session.Message
	FollowUp []session.Message
	Err      *TurnError
}

// --- Model and tool commands ---

// SetModelCmd switches the active model. Idle changes are durable before the
// live value changes.
type SetModelCmd struct {
	Model llm.Model
	Reply chan<- error
}

func (SetModelCmd) command() {}

// SetThinkingCmd changes the thinking level.
type SetThinkingCmd struct {
	Ctx   context.Context
	Level session.ThinkingLevel
	Reply chan<- error
}

func (SetThinkingCmd) command() {}

// SetToolsCmd updates the complete tool registry and active set.
type SetToolsCmd struct {
	Tools  []Tool
	Active []string
	Reply  chan<- error
}

func (SetToolsCmd) command() {}

// ActivateToolsCmd adds registered tools to the active set.
type ActivateToolsCmd struct {
	Ctx   context.Context
	Names []string
	Reply chan<- error
}

func (ActivateToolsCmd) command() {}

// --- Session administration commands ---

// SubscribeCmd opens an independent bounded event stream.
type SubscribeCmd struct {
	Ctx   context.Context
	After EventCursor
	Reply chan<- SubscribeResult
}

func (SubscribeCmd) command() {}

type SubscribeResult struct {
	Sub *EventSubscription
	Err error
}

// CompactCmd requests a context compaction at a safe runtime boundary.
type CompactCmd struct {
	Ctx   context.Context
	Reply chan<- error
}

func (CompactCmd) command() {}

// NavigateCmd moves the active session leaf.
type NavigateCmd struct {
	Ctx    context.Context
	Target string
	Opts   NavigateOptions
	Reply  chan<- NavigateCmdResult
}

func (NavigateCmd) command() {}

type NavigateCmdResult struct {
	Result NavigateResult
	Err    error
}

// AppendSessionInfoCmd updates display metadata for the active session.
type AppendSessionInfoCmd struct {
	Ctx   context.Context
	Name  string
	Reply chan<- SessionInfoResult
}

func (AppendSessionInfoCmd) command() {}

type SessionInfoResult struct {
	Name string
	Err  error
}

// AppendLabelCmd writes a branch label.
type AppendLabelCmd struct {
	Ctx    context.Context
	Target string
	Label  string
	Reply  chan<- SessionInfoResult
}

func (AppendLabelCmd) command() {}

// GetLabelCmd reads a branch label.
type GetLabelCmd struct {
	Ctx    context.Context
	Target string
	Reply  chan<- SessionInfoResult
}

func (GetLabelCmd) command() {}

// AppendMessageCmd appends a message directly (used by template prompts).
type AppendMessageCmd struct {
	Ctx     context.Context
	Message session.Message
	Reply   chan<- error
}

func (AppendMessageCmd) command() {}

// PersistEntryCmd persists a non-turn entry through the controller.
type PersistEntryCmd struct {
	Ctx   context.Context
	Entry session.Entry
	Reply chan<- error
}

func (PersistEntryCmd) command() {}

// ForkSessionCmd creates a new session rooted at a source.
type ForkSessionCmd struct {
	Ctx      context.Context
	SourceID string
	Reply    chan<- ForkResult
}

func (ForkSessionCmd) command() {}

type ForkResult struct {
	ID  string
	Err error
}

// commandResult is a helper for sending exactly one result on a reply channel.
func sendResult[T any](reply chan<- T, result T) {
	if reply == nil {
		return
	}
	reply <- result
}
