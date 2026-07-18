package agent

import (
	"context"
	"errors"

	ionexport "github.com/nijaru/ion/internal/export"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

const controllerCommandCapacity = 128

var ErrCommandQueueFull = errors.New("runtime command queue is full")

type runtimeCommand struct {
	start func()
}

// commandLoop owns acceptance of long-running turn commands. The turn worker
// performs provider/storage work outside the command owner; its completion is
// returned through the request channel after the controller's lifecycle defer
// has reached the idle boundary.
func (h *Harness) commandLoop() {
	for {
		select {
		case command := <-h.commands:
			if command.start != nil {
				command.start()
			}
		case <-h.commandStop:
			h.rejectQueuedCommands()
			return
		}
	}
}

func (h *Harness) rejectQueuedCommands() {
	for {
		select {
		case command := <-h.commands:
			if command.start != nil {
				command.start()
			}
		default:
			return
		}
	}
}

type promptCommand struct {
	ctx    context.Context
	text   string
	images []session.ImageContent
	reply  chan promptResult
}

type promptResult struct {
	message session.Message
	err     error
}

func (h *Harness) enqueueCommand(ctx context.Context, command runtimeCommand) error {
	if ctx != nil {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}
	}
	h.commandMu.Lock()
	defer h.commandMu.Unlock()
	h.mu.Lock()
	closed := h.closed
	h.mu.Unlock()
	if closed {
		return errors.New("harness is closed")
	}
	select {
	case h.commands <- command:
		return nil
	default:
		return ErrCommandQueueFull
	}
}

func (h *Harness) submitImmediate(ctx context.Context, fn func() error) error {
	reply := make(chan error, 1)
	if err := h.enqueueCommand(ctx, runtimeCommand{start: func() { reply <- fn() }}); err != nil {
		return err
	}
	return <-reply
}

func (h *Harness) submitAsync(ctx context.Context, fn func() error) error {
	reply := make(chan error, 1)
	if err := h.enqueueCommand(ctx, runtimeCommand{start: func() {
		go func() { reply <- fn() }()
	}}); err != nil {
		return err
	}
	return <-reply
}

type controllerResult[T any] struct {
	value T
	err   error
}

func submitResult[T any](h *Harness, ctx context.Context, fn func() (T, error)) (T, error) {
	var zero T
	reply := make(chan controllerResult[T], 1)
	if err := h.enqueueCommand(ctx, runtimeCommand{start: func() {
		go func() {
			value, err := fn()
			reply <- controllerResult[T]{value: value, err: err}
		}()
	}}); err != nil {
		return zero, err
	}
	result := <-reply
	return result.value, result.err
}

// Steer queues a message through the controller owner.
func (h *Harness) Steer(text string, images ...session.ImageContent) error {
	return h.submitImmediate(context.Background(), func() error {
		return h.steerDirect(text, images...)
	})
}

// FollowUp queues a message through the controller owner.
func (h *Harness) FollowUp(text string, images ...session.ImageContent) error {
	return h.submitImmediate(context.Background(), func() error {
		return h.followUpDirect(text, images...)
	})
}

// NextTurn queues a message through the controller owner.
func (h *Harness) NextTurn(text string, images ...session.ImageContent) error {
	return h.submitImmediate(context.Background(), func() error {
		return h.nextTurnDirect(text, images...)
	})
}

// SetModel updates model state through the controller owner.
func (h *Harness) SetModel(model llm.Model) error {
	return h.submitImmediate(context.Background(), func() error {
		return h.setModelDirect(model)
	})
}

// SetTools updates the tool registry through the controller owner.
func (h *Harness) SetTools(tools []Tool, active []string) error {
	return h.submitImmediate(context.Background(), func() error {
		return h.setToolsDirect(tools, active)
	})
}

// SetThinking accepts the command without making the controller loop perform
// session I/O inline.
func (h *Harness) SetThinking(ctx context.Context, level session.ThinkingLevel) error {
	return h.submitAsync(ctx, func() error {
		return h.setThinkingDirect(ctx, level)
	})
}

// ActivateTools accepts the command without making the controller loop
// perform session I/O inline.
func (h *Harness) ActivateTools(ctx context.Context, names []string) error {
	return h.submitAsync(ctx, func() error {
		return h.activateToolsDirect(ctx, names)
	})
}

func (h *Harness) ExportSessionBundle(ctx context.Context, sessionID string) (ionexport.SessionBundle, error) {
	return submitResult(h, ctx, func() (ionexport.SessionBundle, error) {
		return h.exportSessionBundleDirect(ctx, sessionID)
	})
}

func (h *Harness) ImportSessionBundle(ctx context.Context, bundle ionexport.SessionBundle) (string, error) {
	return submitResult(h, ctx, func() (string, error) {
		return h.importSessionBundleDirect(ctx, bundle)
	})
}

func (h *Harness) ForkSession(ctx context.Context, sourceID string) (string, error) {
	return submitResult(h, ctx, func() (string, error) {
		return h.forkSessionDirect(ctx, sourceID)
	})
}

func (h *Harness) Compact(ctx context.Context) error {
	return h.submitAsync(ctx, func() error {
		return h.compactDirect(ctx)
	})
}

func (h *Harness) AppendMessage(ctx context.Context, msg session.Message) error {
	return h.submitAsync(ctx, func() error {
		return h.appendMessageDirect(ctx, msg)
	})
}

func (h *Harness) PersistEntry(ctx context.Context, entry session.Entry) error {
	return h.submitAsync(ctx, func() error {
		return h.persistEntryDirect(ctx, entry)
	})
}

func (h *Harness) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	return submitResult(h, ctx, func() (string, error) {
		return h.appendSessionInfoDirect(ctx, name)
	})
}

func (h *Harness) AppendLabel(ctx context.Context, targetID, label string) (string, error) {
	return submitResult(h, ctx, func() (string, error) {
		return h.appendLabelDirect(ctx, targetID, label)
	})
}

func (h *Harness) GetLabel(ctx context.Context, targetID string) (string, error) {
	return submitResult(h, ctx, func() (string, error) {
		return h.getLabelDirect(ctx, targetID)
	})
}

func (h *Harness) NavigateTree(ctx context.Context, targetID string, opts NavigateOptions) (NavigateResult, error) {
	return submitResult(h, ctx, func() (NavigateResult, error) {
		return h.navigateTreeDirect(ctx, targetID, opts)
	})
}

// submitPrompt is the only public-to-controller acceptance path for a turn.
// Once the command is accepted, caller cancellation is carried by the turn
// context and the caller waits for the controller's terminal result instead of
// abandoning cleanup midway through persistence or durable abort.
func (h *Harness) submitPrompt(ctx context.Context, text string, images []session.ImageContent) (session.Message, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	request := &promptCommand{
		ctx:    ctx,
		text:   text,
		images: cloneImageContents(images),
		reply:  make(chan promptResult, 1),
	}
	command := runtimeCommand{start: func() { h.startPrompt(request) }}
	if err := h.enqueueCommand(ctx, command); err != nil {
		return nil, err
	}
	result := <-request.reply
	return result.message, result.err
}

func (h *Harness) startPrompt(request *promptCommand) {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		request.reply <- promptResult{err: errors.New("harness is closed")}
		return
	}
	if h.phase != PhaseIdle {
		phase := h.phase
		h.mu.Unlock()
		request.reply <- promptResult{err: busyError(phase)}
		return
	}
	h.phase = PhaseTurn
	h.runDone = make(chan struct{})
	h.runCancel = make(chan struct{})
	h.mu.Unlock()

	go func() {
		message, err := h.runPrompt(request.ctx, request.text, request.images...)
		request.reply <- promptResult{message: message, err: err}
	}()
}

func busyError(phase Phase) error {
	return &RuntimeStateError{State: phase, Err: errors.New("runtime is busy")}
}

// RuntimeStateError distinguishes a rejected command from a provider or
// persistence failure without forcing callers to parse strings.
type RuntimeStateError struct {
	State Phase
	Err   error
}

func (e *RuntimeStateError) Error() string {
	if e == nil || e.Err == nil {
		return string(e.State)
	}
	return "runtime state " + string(e.State) + ": " + e.Err.Error()
}

func (e *RuntimeStateError) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.Err
}
