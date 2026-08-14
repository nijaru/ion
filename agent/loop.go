package agent

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
	"sync"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// loopResult is the stateless turn engine result used by the controller.
// ReplaySafeContextOverflow is true only when the provider reported context
// overflow before any response chunk or external tool effect crossed the loop
// boundary. The controller may compact and replay only at that safe frontier.
type loopResult struct {
	messages                  []session.Message
	replaySafeContextOverflow bool
}

// RunLoop is the stateless turn engine. All inputs are arguments.
// Emits events via emit. Returns new messages produced during the run.
// Failure is terminal: on error/abort, synthesize a failure AssistantMessage,
// emit TurnEnd + AgentEnd, and return.
//
// Reference: Pi agent-loop.js runLoop (line 77).
func RunLoop(
	ctx context.Context,
	prompts []session.Message,
	snapshot TurnContext,
	cfg LoopConfig,
	emit func(session.Event),
	signal <-chan struct{},
) []session.Message {
	result := runLoop(ctx, prompts, snapshot, cfg, emit, signal)
	if result.replaySafeContextOverflow {
		for i := len(result.messages) - 1; i >= 0; i-- {
			assistant, ok := result.messages[i].(*session.AssistantMessage)
			if !ok {
				continue
			}
			emit(session.MessageStart{Message: assistant})
			emit(session.MessageEnd{Message: assistant})
			emit(session.TurnEnd{Message: *assistant})
			emit(session.AgentEnd{Messages: result.messages})
			break
		}
	}
	return result.messages
}

func runLoop(
	ctx context.Context,
	prompts []session.Message,
	snapshot TurnContext,
	cfg LoopConfig,
	emit func(session.Event),
	signal <-chan struct{},
) loopResult {
	convert := cfg.Convert
	if convert == nil {
		convert = DefaultConvert
	}

	runCtx, cancelRun := contextWithSignal(ctx, signal)
	defer cancelRun()

	newMessages := make([]session.Message, 0, len(prompts))
	currentCtx := TurnContext{
		SystemPrompt: snapshot.SystemPrompt,
		Messages:     append([]session.Message{}, snapshot.Messages...),
	}

	emit(session.AgentStart{Origin: cfg.Origin})
	emit(session.TurnStart{})

	// Emit prompt messages (Pi: message_start + message_end for each prompt).
	for _, p := range prompts {
		emit(session.MessageStart{Message: p})
		emit(session.MessageEnd{Message: p})
		newMessages = append(newMessages, p)
		currentCtx.Messages = append(currentCtx.Messages, p)
	}

	firstTurn := true
	replaySafe := true

	// Drain initial steering messages.
	pending := drain(cfg.DrainSteer)

	maxIter := cfg.MaxToolIterations
	if maxIter <= 0 {
		maxIter = 25 // safety default
	}
	innerIter := 0

	// Outer loop: continues when follow-up messages arrive after agent would stop.
	for {
		hasMoreToolCalls := true

		// Inner loop: process tool calls and steering messages.
		for hasMoreToolCalls || len(pending) > 0 {
			innerIter++
			if innerIter > maxIter {
				msg := newFailureMessage(
					cfg.Model,
					fmt.Errorf("max tool iterations (%d) exceeded", maxIter),
					false,
					cfg.Thinking,
				)
				newMessages = append(newMessages, &msg)
				emit(session.MessageStart{Message: &msg})
				emit(session.MessageEnd{Message: &msg})
				emit(session.TurnEnd{Message: msg})
				emit(session.AgentEnd{Messages: newMessages})
				return loopResult{messages: newMessages}
			}
			if !firstTurn {
				emit(session.TurnStart{})
			}
			firstTurn = false

			// Inject pending messages (steering or follow-up).
			for _, msg := range pending {
				emit(session.MessageStart{Message: msg})
				emit(session.MessageEnd{Message: msg})
				currentCtx.Messages = append(currentCtx.Messages, msg)
				newMessages = append(newMessages, msg)
			}
			pending = nil

			// Stream assistant response.
			response := streamAssistantResponse(runCtx, currentCtx, cfg, convert, emit, signal)
			assistantMsg, aborted := response.message, response.aborted
			if response.providerProgress || !response.aborted {
				// Any completed assistant response establishes a visible
				// turn boundary, even when the provider yielded no chunks.
				// A later overflow must not replay past accepted follow-ups.
				replaySafe = false
			}
			newMessages = append(newMessages, assistantMsg)
			currentCtx.Messages = append(currentCtx.Messages, assistantMsg)

			if aborted {
				// A replay-safe overflow is an internal recovery boundary, not a
				// terminal logical turn. Do not publish TurnEnd/AgentEnd or the
				// transient failure message before compaction decides the retry.
				replaySafeOverflow := replaySafe && response.replaySafeOverflow
				if !replaySafeOverflow {
					emit(session.TurnEnd{Message: assistantMsg})
					emit(session.AgentEnd{Messages: newMessages})
				}
				return loopResult{
					messages:                  newMessages,
					replaySafeContextOverflow: replaySafeOverflow,
				}
			}

			// Providers can finish a clean stream with an error/aborted stop
			// reason instead of returning a Go error. Treat that terminal response
			// exactly like the explicit failure path above; in particular, never
			// execute tool calls that arrived alongside a failed response.
			if assistantMsg.StopReason == session.StopReasonError ||
				assistantMsg.StopReason == session.StopReasonAborted {
				emit(session.TurnEnd{Message: assistantMsg})
				emit(session.AgentEnd{Messages: newMessages})
				return loopResult{messages: newMessages}
			}

			// Check for tool calls in the assistant response.
			var toolCalls []*session.ToolCall
			for _, c := range assistantMsg.Content {
				if tc, ok := c.(*session.ToolCall); ok {
					toolCalls = append(toolCalls, tc)
				}
			}

			var toolResults []session.ToolResultMessage
			hasMoreToolCalls = false

			if len(toolCalls) > 0 {
				if assistantMsg.StopReason == session.StopReasonLength {
					// Output token limit truncated the response, so streamed tool-call
					// arguments are garbage. Fail them instead of executing — Pi's
					// failToolCallsFromTruncatedMessage (agent-loop.js:383).
					for _, tc := range toolCalls {
						emit(session.ToolExecStart{ToolCallID: tc.ID, Name: tc.Name})
						res := session.ToolResultMessage{
							ToolCallID: tc.ID,
							ToolName:   tc.Name,
							Content: []session.Content{
								session.TextContent{
									Text: "The model response was truncated by the output token limit; tool call was not executed.",
								},
							},
							IsError:   true,
							Timestamp: time.Now(),
						}
						emit(session.ToolExecEnd{ToolCallID: tc.ID, Result: res})
						toolResults = append(toolResults, res)
						currentCtx.Messages = append(currentCtx.Messages, &res)
						newMessages = append(newMessages, &res)
					}
				} else {
					results, terminate := executeToolCalls(
						runCtx,
						currentCtx,
						*assistantMsg,
						toolCalls,
						cfg,
						emit,
						signal,
					)
					toolResults = results
					hasMoreToolCalls = !terminate

					// Pi: emit message_start + message_end for each tool result message.
					// Reference: Pi agent-loop.js emitToolResultMessage (line 514).
					for i := range toolResults {
						result := &toolResults[i]
						emit(session.MessageStart{Message: result})
						emit(session.MessageEnd{Message: result})
						currentCtx.Messages = append(currentCtx.Messages, result)
						newMessages = append(newMessages, result)
					}
				}
			}

			emit(session.TurnEnd{Message: assistantMsg, ToolResults: toolResults})

			// Prepare next turn (harness flushes writes, rebuilds context).
			if cfg.PrepareNextTurn != nil {
				snap := cfg.PrepareNextTurn(runCtx, toolResults)
				if snap != nil {
					if snap.Context.Messages != nil {
						currentCtx = snap.Context
					}
					if snap.Model != nil {
						cfg.Model = *snap.Model
					}
					if snap.Thinking != nil {
						cfg.Thinking = *snap.Thinking
					}
					if snap.Tools != nil {
						cfg.Tools = snap.Tools
					}
				}
			}

			// Check if the agent should stop.
			if cfg.ShouldStop != nil && cfg.ShouldStop(StopContext{
				Message:     *assistantMsg,
				ToolResults: toolResults,
				Context:     currentCtx,
			}) {
				emit(session.AgentEnd{Messages: newMessages})
				return loopResult{messages: newMessages}
			}

			// Drain steering messages for next inner iteration.
			pending = drain(cfg.DrainSteer)
		}

		// Agent would stop here. Check for follow-up messages.
		followUps := drain(cfg.DrainFollowUp)
		if len(followUps) > 0 {
			pending = followUps
			continue
		}

		break
	}

	emit(session.AgentEnd{Messages: newMessages})
	return loopResult{messages: newMessages}
}

func isContextOverflow(cfg LoopConfig, err error) bool {
	if err == nil || llm.IsStreamCleanupError(err) || isHookError(err) {
		return false
	}
	if cfg.ContextOverflow != nil && cfg.ContextOverflow(err) {
		return true
	}
	return IsContextOverflowError(err)
}

func isHookError(err error) bool {
	var hookErr *hookError
	return errors.As(err, &hookErr)
}

// streamResult describes one provider response for the controller-owned
// recovery frontier. A response can be replayed for context overflow only if
// no provider chunk was observed and the run had not already made progress.
type streamResult struct {
	message            *session.AssistantMessage
	aborted            bool
	replaySafeOverflow bool
	providerProgress   bool
}

// streamAssistantResponse calls the LLM and accumulates the response into an AssistantMessage.
// Returns the message and whether the stream was aborted.
//
// Reference: Pi agent-loop.js streamAssistantResponse (line 172).
func streamAssistantResponse(
	ctx context.Context,
	snapshot TurnContext,
	cfg LoopConfig,
	convert func([]session.Message) []llm.Message,
	emit func(session.Event),
	signal <-chan struct{},
) streamResult {
	if isCanceled(ctx, signal) {
		msg := newFailureMessage(cfg.Model, context.Canceled, true, cfg.Thinking)
		msg.Error = "response aborted"
		emit(session.MessageStart{Message: &msg})
		emit(session.MessageEnd{Message: &msg})
		return streamResult{message: &msg, aborted: true}
	}

	// Derive a cancellable context from the signal channel so the
	// provider stream is cancelled when the run is aborted.
	streamCtx, cancelStream := contextWithSignal(ctx, signal)
	defer cancelStream()

	msgs := snapshot.Messages
	// Tier-1 Micro-compaction: prune verbose tool output on older turns when configured.
	if cfg.Compaction.MicroEnabled {
		msgs = MicroCompactMessages(msgs, cfg.Compaction.MicroKeepRecentTurns, cfg.Compaction.MicroMaxToolChars)
	}
	if cfg.TransformCtx != nil {
		msgs = cfg.TransformCtx(streamCtx, msgs)
	}
	if cfg.ContextUsageTracker != nil && cfg.ContextWindow > 0 {
		tokens := 0
		for _, m := range msgs {
			tokens += EstimateTokens(m)
		}
		if hint, ok := cfg.ContextUsageTracker.MaybeInjectHint(tokens, cfg.ContextWindow); ok && hint != "" {
			msgs = append(msgs, session.NewUserText(hint, time.Now()))
		}
	}

	llmMsgs := convert(msgs)

	cachePrefix := 0
	// Prepend system prompt as a system message if present.
	// Pi stores systemPrompt in the context object; the model resolver layer
	// injects it. Ion builds requests directly, so prepend here.
	if snapshot.SystemPrompt != "" {
		sysMsg := llm.Message{
			Role:    llm.RoleSystem,
			Content: snapshot.SystemPrompt,
		}
		llmMsgs = append([]llm.Message{sysMsg}, llmMsgs...)
		cachePrefix = 1
	}
	tools := make([]*llm.Spec, len(cfg.Tools))
	for i, t := range cfg.Tools {
		tools[i] = &llm.Spec{
			Name:        t.Name,
			Description: t.Description,
			Parameters:  t.Parameters,
		}
	}
	if len(tools) > 0 {
		tools[len(tools)-1].CacheControl = &llm.CacheControl{Type: "ephemeral"}
	} else if len(llmMsgs) > 0 && llmMsgs[0].Role == llm.RoleSystem {
		llmMsgs[0].CacheControl = &llm.CacheControl{Type: "ephemeral"}
	}
	if len(llmMsgs) >= 3 {
		llmMsgs[len(llmMsgs)-2].CacheControl = &llm.CacheControl{Type: "ephemeral"}
	}

	req := &llm.Request{
		Model:               cfg.Model.ID,
		Messages:            llmMsgs,
		Tools:               tools,
		CachePrefixMessages: cachePrefix,
		ReasoningEffort:     providerReasoningEffort(cfg.Thinking),
		SessionID:           cfg.SessionID,
	}

	if cfg.Auth != nil {
		key, headers := cfg.Auth(cfg.Model)
		if key != "" {
			if req.Headers == nil {
				req.Headers = make(map[string]string)
			}
			req.Headers["Authorization"] = "Bearer " + key
		}
		for k, v := range headers {
			if req.Headers == nil {
				req.Headers = make(map[string]string)
			}
			req.Headers[k] = v
		}
		req.Model = cfg.Model.ID
	}

	if cfg.StreamFn == nil {
		err := fmt.Errorf("stream function is not configured")
		msg := newFailureMessage(cfg.Model, err, false, cfg.Thinking)
		emit(session.MessageStart{Message: &msg})
		emit(session.MessageEnd{Message: &msg})
		return streamResult{message: &msg, aborted: true}
	}

	stream, err := cfg.StreamFn(streamCtx, req)
	if err == nil && stream == nil {
		err = fmt.Errorf("provider returned a nil stream")
	}
	if err != nil {
		var cleanupErr error
		if stream != nil {
			cleanupErr = errors.Join(stream.Close(), stream.Err())
		}
		if isCanceled(ctx, signal) {
			if cleanupErr != nil {
				err = errors.Join(err, cleanupErr)
			}
			msg := newFailureMessage(cfg.Model, fmt.Errorf("response aborted: %w", err), true, cfg.Thinking)
			emit(session.MessageStart{Message: &msg})
			emit(session.MessageEnd{Message: &msg})
			return streamResult{message: &msg, aborted: true}
		}
		replaySafeOverflow := isContextOverflow(cfg, err) && cleanupErr == nil
		if replaySafeOverflow {
			// Keep this internal recovery attempt invisible. The controller
			// will compact and retry without publishing a transient failure.
			msg := newFailureMessage(cfg.Model, fmt.Errorf("context_length_exceeded: %w", err), false, cfg.Thinking)
			return streamResult{
				message:            &msg,
				aborted:            true,
				replaySafeOverflow: true,
			}
		}
		if cleanupErr != nil {
			err = errors.Join(err, cleanupErr)
		}
		msg := newFailureMessage(cfg.Model, err, false, cfg.Thinking)
		emit(session.MessageStart{Message: &msg})
		emit(session.MessageEnd{Message: &msg})
		return streamResult{message: &msg, aborted: true}
	}
	var acc llm.StreamAccumulator
	started := false
	providerProgress := false
	var streamErr error
	previousToolArguments := make(map[string]string)

	for {
		chunk, ok := stream.Next()
		if !ok {
			break
		}
		if chunk == nil {
			streamErr = errors.New("provider returned a nil stream chunk")
			break
		}

		acc.Add(chunk)
		providerProgress = true

		// Build partial message from accumulator state.
		partial := buildPartialMessage(acc, cfg.Model)
		if !started {
			emit(session.MessageStart{Message: &partial})
			started = true
		}

		emitStreamChunkUpdates(&partial, chunk, previousToolArguments, emit)
	}

	// Close is part of the provider boundary. A stream that cannot be closed
	// cleanly is not a successful assistant response, even after it yielded EOF.
	iterationErr := streamErr
	closeErr := stream.Close()
	providerErr := stream.Err()
	streamErr = errors.Join(iterationErr, providerErr, closeErr)

	if isCanceled(ctx, signal) {
		// Cancellation may surface as ok=false with a nil stream error;
		// treat it as an aborted turn, not a completed one. Pi agent-loop.js abort branch.
		final := buildPartialMessage(acc, cfg.Model)
		final.ThinkingLevel = cfg.Thinking
		final.StopReason = session.StopReasonAborted
		final.Error = "response aborted"
		if streamErr != nil {
			final.Error = fmt.Sprintf("response aborted: %v", streamErr)
		}
		if !started {
			emit(session.MessageStart{Message: &final})
		}
		emit(session.MessageEnd{Message: &final})
		return streamResult{message: &final, aborted: true, providerProgress: providerProgress}
	}
	if streamErr != nil {
		replaySafeOverflow := iterationErr == nil &&
			isContextOverflow(cfg, providerErr) && !providerProgress && closeErr == nil
		if replaySafeOverflow {
			msg := buildFailureAssistantMessage(
				acc,
				cfg.Model,
				cfg.Thinking,
				fmt.Errorf("context_length_exceeded: %w", streamErr),
			)
			return streamResult{
				message:            &msg,
				aborted:            true,
				replaySafeOverflow: true,
			}
		}
		if isContextOverflow(cfg, streamErr) {
			streamErr = fmt.Errorf("context_length_exceeded: %w", streamErr)
		}
		msg := buildFailureAssistantMessage(acc, cfg.Model, cfg.Thinking, streamErr)
		if !started {
			emit(session.MessageStart{Message: &msg})
		}
		emit(session.MessageEnd{Message: &msg})
		return streamResult{message: &msg, aborted: true, providerProgress: providerProgress}
	}

	// Build final message from accumulator. Malformed tool arguments are a
	// provider failure, not an empty argument object that may execute.
	final, buildErr := buildAssistantMessage(acc, cfg.Model, cfg.Thinking)
	if buildErr != nil {
		msg := buildFailureAssistantMessage(acc, cfg.Model, cfg.Thinking, buildErr)
		if !started {
			emit(session.MessageStart{Message: &msg})
		}
		emit(session.MessageEnd{Message: &msg})
		return streamResult{message: &msg, aborted: true, providerProgress: providerProgress}
	}
	if !started {
		emit(session.MessageStart{Message: &final})
	}
	emit(session.MessageEnd{Message: &final})
	return streamResult{message: &final, providerProgress: providerProgress}
}

func emitStreamChunkUpdates(
	partial *session.AssistantMessage,
	chunk *llm.Chunk,
	previousToolArguments map[string]string,
	emit func(session.Event),
) {
	if chunk == nil {
		return
	}
	textEmitted := false
	thinkingEmitted := false
	emittedToolCalls := make(map[string]struct{})
	switch block := chunk.Block.(type) {
	case llm.TextBlock:
		if block.Text != "" {
			emit(session.MessageUpdate{
				Message:   partial,
				Delta:     session.TextDelta{Text: block.Text},
				BlockType: "text",
			})
			textEmitted = true
		}
	case llm.ThinkingBlock:
		if block.Thinking != "" {
			emit(session.MessageUpdate{
				Message:   partial,
				Delta:     session.ThinkingDelta{Text: block.Thinking},
				BlockType: "thinking",
			})
			thinkingEmitted = true
		}
	case llm.ToolCallBlock:
		emitToolCallDelta(partial, block.ID, block.Name, block.Arguments, previousToolArguments, emit)
		if block.ID != "" {
			emittedToolCalls[block.ID] = struct{}{}
		}
	}
	if !textEmitted && chunk.Content != "" {
		emit(session.MessageUpdate{
			Message:   partial,
			Delta:     session.TextDelta{Text: chunk.Content},
			BlockType: "text",
		})
	}
	if !thinkingEmitted {
		if chunk.Reasoning != "" {
			emit(session.MessageUpdate{
				Message:   partial,
				Delta:     session.ThinkingDelta{Text: chunk.Reasoning},
				BlockType: "thinking",
			})
		} else if _, typed := chunk.Block.(llm.ThinkingBlock); !typed {
			for _, block := range chunk.ThinkingBlocks {
				if block.Thinking == "" {
					continue
				}
				emit(session.MessageUpdate{
					Message:   partial,
					Delta:     session.ThinkingDelta{Text: block.Thinking},
					BlockType: "thinking",
				})
			}
		}
	}
	for _, call := range chunk.Calls {
		if _, emitted := emittedToolCalls[call.ID]; emitted {
			continue
		}
		emitToolCallDelta(
			partial,
			call.ID,
			call.Function.Name,
			call.Function.Arguments,
			previousToolArguments,
			emit,
		)
	}
}

func emitToolCallDelta(
	partial *session.AssistantMessage,
	id, name, arguments string,
	previousToolArguments map[string]string,
	emit func(session.Event),
) {
	previous := previousToolArguments[id]
	chunk := arguments
	if strings.HasPrefix(arguments, previous) {
		chunk = arguments[len(previous):]
	}
	previousToolArguments[id] = arguments
	emit(session.MessageUpdate{
		Message: partial,
		Delta: session.ToolCallDelta{
			ToolCallID:     id,
			Name:           name,
			ArgumentsChunk: chunk,
		},
		BlockType: "tool_call",
	})
}

// executeToolCalls executes tool calls from an assistant message.
//
// Reference: Pi agent-loop.js executeToolCalls (line 254).
func executeToolCalls(
	ctx context.Context,
	snapshot TurnContext,
	assistantMsg session.AssistantMessage,
	toolCalls []*session.ToolCall,
	cfg LoopConfig,
	emit func(session.Event),
	signal <-chan struct{},
) ([]session.ToolResultMessage, bool) {
	hasSequential := false
	for _, tc := range toolCalls {
		for _, t := range cfg.Tools {
			if t.Name == tc.Name && t.ExecutionMode == ExecSequential {
				hasSequential = true
				break
			}
		}
	}

	if hasSequential {
		return executeToolCallsSequential(ctx, snapshot, assistantMsg, toolCalls, cfg, emit, signal)
	}
	return executeToolCallsParallel(ctx, snapshot, assistantMsg, toolCalls, cfg, emit, signal)
}

type toolProgressGate struct {
	stateMu sync.Mutex
	emitMu  *sync.Mutex
	emit    func(session.Event)
	open    bool
}

func newToolProgressGate(emit func(session.Event), emitMu *sync.Mutex) *toolProgressGate {
	return &toolProgressGate{emitMu: emitMu, emit: emit, open: true}
}

func (g *toolProgressGate) emitUpdate(event session.Event) {
	g.stateMu.Lock()
	defer g.stateMu.Unlock()
	if !g.open {
		return
	}
	g.emitMu.Lock()
	defer g.emitMu.Unlock()
	g.emit(event)
}

func (g *toolProgressGate) close() {
	g.stateMu.Lock()
	g.open = false
	g.stateMu.Unlock()
}

func executeToolCallsSequential(
	ctx context.Context,
	snapshot TurnContext,
	assistantMsg session.AssistantMessage,
	toolCalls []*session.ToolCall,
	cfg LoopConfig,
	emit func(session.Event),
	signal <-chan struct{},
) ([]session.ToolResultMessage, bool) {
	var results []session.ToolResultMessage
	progressEmitMu := new(sync.Mutex)

	for _, tc := range toolCalls {
		argsRaw, _ := json.Marshal(tc.Arguments)
		emit(session.ToolExecStart{ToolCallID: tc.ID, Name: tc.Name, Args: argsRaw})
		prepared, preparationResult := prepareToolCall(ctx, snapshot, assistantMsg, tc, cfg, signal)
		var result session.ToolResultMessage
		if preparationResult != nil {
			result = *preparationResult
			emit(session.ToolExecEnd{ToolCallID: tc.ID, Result: result})
		} else {
			progress := newToolProgressGate(emit, progressEmitMu)
			result = executePreparedToolCall(ctx, snapshot, assistantMsg, prepared, cfg, func(event session.Event) {
				if _, end := event.(session.ToolExecEnd); end {
					progress.close()
				}
				if _, live := event.(session.ToolExecUpdate); live {
					progress.emitUpdate(event)
					return
				}
				emit(event)
			}, signal)
			progress.close()
		}
		results = append(results, result)
		if isCanceled(ctx, signal) {
			break
		}
	}

	// Pi: terminate when every finalized call has result.terminate === true.
	// Reference: Pi agent-loop.js shouldTerminateToolBatch (line 345).
	return results, shouldTerminateToolBatch(results)
}

func shouldTerminateToolBatch(results []session.ToolResultMessage) bool {
	if len(results) == 0 {
		return false
	}
	for _, r := range results {
		if !r.Terminate {
			return false
		}
	}
	return true
}

// preparedToolCall holds a tool call that has passed the sequential
// preparation phase (find, prepareArgs, validate, beforeToolCall).
// Only these are eligible for concurrent execution.
type preparedToolCall struct {
	index   int
	tool    *Tool
	tc      *session.ToolCall
	argsRaw json.RawMessage
	action  *ActionToken
}

func invokeBeforeToolCall(cfg LoopConfig, call ToolCallContext) (decision *ToolCallDecision, err error) {
	if cfg.BeforeToolCall == nil {
		return nil, nil
	}
	defer func() {
		if recovered := recover(); recovered != nil {
			err = fmt.Errorf("before tool call panic: %v", recovered)
		}
	}()
	return cfg.BeforeToolCall(call), nil
}

func prepareToolArguments(tool *Tool, arguments map[string]any) (args map[string]any, err error) {
	args = arguments
	if tool.PrepareArgs == nil {
		return args, nil
	}
	defer func() {
		if recovered := recover(); recovered != nil {
			err = fmt.Errorf("prepare arguments panic: %v", recovered)
		}
	}()
	raw, err := json.Marshal(arguments)
	if err != nil {
		return arguments, fmt.Errorf("marshal arguments for preparation: %w", err)
	}
	prepared := tool.PrepareArgs(raw)
	decoder := json.NewDecoder(strings.NewReader(string(prepared)))
	decoder.UseNumber()
	if err := decoder.Decode(&args); err != nil {
		return arguments, fmt.Errorf("decode prepared arguments: %w", err)
	}
	var trailing any
	if err := decoder.Decode(&trailing); err != io.EOF {
		if err == nil {
			return arguments, fmt.Errorf("decode prepared arguments: trailing JSON value")
		}
		return arguments, fmt.Errorf("decode prepared arguments: %w", err)
	}
	return args, nil
}

func abortedToolResult(tc *session.ToolCall) *session.ToolResultMessage {
	return &session.ToolResultMessage{
		ToolCallID: tc.ID,
		ToolName:   tc.Name,
		Content:    []session.Content{session.TextContent{Text: "Operation aborted"}},
		IsError:    true,
		Timestamp:  time.Now(),
	}
}

// prepareToolCall runs the sequential portion of tool preparation: find the
// tool, normalize args, validate schema, and run the before_tool_call hook.
// Returns the prepared call on success, or a non-nil result on failure.
// Does NOT emit any events — the caller decides what to emit.
func prepareToolCall(
	ctx context.Context,
	snapshot TurnContext,
	assistantMsg session.AssistantMessage,
	tc *session.ToolCall,
	cfg LoopConfig,
	signal <-chan struct{},
) (preparedToolCall, *session.ToolResultMessage) {
	// Find the tool.
	var tool *Tool
	for i := range cfg.Tools {
		if cfg.Tools[i].Name == tc.Name {
			tool = &cfg.Tools[i]
			break
		}
	}
	if tool == nil {
		return preparedToolCall{}, &session.ToolResultMessage{
			ToolCallID: tc.ID, ToolName: tc.Name,
			Content: []session.Content{session.TextContent{Text: "tool not found: " + tc.Name}},
			IsError: true, Timestamp: time.Now(),
		}
	}
	if isCanceled(ctx, signal) {
		return preparedToolCall{}, abortedToolResult(tc)
	}

	// Pi: prepareToolCallArguments normalizes args before validation.
	// Reference: Pi agent-loop.js prepareToolCallArguments (line 360).
	args, prepareErr := prepareToolArguments(tool, tc.Arguments)
	if prepareErr != nil {
		return preparedToolCall{}, &session.ToolResultMessage{
			ToolCallID: tc.ID,
			ToolName:   tc.Name,
			Content: []session.Content{
				session.TextContent{Text: fmt.Sprintf("invalid prepared arguments: %v", prepareErr)},
			},
			IsError:   true,
			Timestamp: time.Now(),
		}
	}

	// Pi: validateToolArguments checks schema and returns coerced arguments.
	if tool.Parameters != nil {
		var err error
		args, err = coerceAndValidateArgs(tool.Parameters, args)
		if err != nil {
			return preparedToolCall{}, &session.ToolResultMessage{
				ToolCallID: tc.ID, ToolName: tc.Name,
				Content: []session.Content{session.TextContent{Text: fmt.Sprintf("invalid arguments: %v", err)}},
				IsError: true, Timestamp: time.Now(),
			}
		}
	}

	argsRaw, marshalErr := json.Marshal(args)
	if marshalErr != nil {
		return preparedToolCall{}, &session.ToolResultMessage{
			ToolCallID: tc.ID, ToolName: tc.Name,
			Content: []session.Content{session.TextContent{Text: fmt.Sprintf("invalid arguments: %v", marshalErr)}},
			IsError: true, Timestamp: time.Now(),
		}
	}

	// BeforeToolCall hook.
	if cfg.BeforeToolCall != nil {
		decision, hookErr := invokeBeforeToolCall(cfg, ToolCallContext{
			RunContext:       ctx,
			AssistantMessage: assistantMsg, ToolCall: tc, Args: argsRaw, Context: snapshot,
		})
		if hookErr != nil {
			return preparedToolCall{}, &session.ToolResultMessage{
				ToolCallID: tc.ID, ToolName: tc.Name,
				Content: []session.Content{session.TextContent{Text: hookErr.Error()}},
				IsError: true, Timestamp: time.Now(),
			}
		}
		if isCanceled(ctx, signal) {
			return preparedToolCall{}, abortedToolResult(tc)
		}
		if decision != nil && decision.Block {
			return preparedToolCall{}, &session.ToolResultMessage{
				ToolCallID: tc.ID, ToolName: tc.Name,
				Content: []session.Content{session.TextContent{Text: decision.Reason}},
				IsError: true, Timestamp: time.Now(),
			}
		}
	}
	if isCanceled(ctx, signal) {
		return preparedToolCall{}, abortedToolResult(tc)
	}
	if tool.RequiresAction || tool.ApprovalRequirement != nil {
		requirement, declared, descriptorErr := invokeActionDescriptor(tool, argsRaw)
		if descriptorErr != nil {
			return preparedToolCall{}, &session.ToolResultMessage{
				ToolCallID: tc.ID, ToolName: tc.Name,
				Content: []session.Content{session.TextContent{Text: descriptorErr.Error()}},
				IsError: true, Timestamp: time.Now(),
			}
		}
		required := declared
		if tool.ApprovalRequirement == nil {
			required = tool.RequiresAction
		}
		if !required {
			return preparedToolCall{tool: tool, tc: tc, argsRaw: argsRaw}, nil
		}
		if isCanceled(ctx, signal) {
			return preparedToolCall{}, abortedToolResult(tc)
		}
		if cfg.ActionBoundary == nil {
			return preparedToolCall{}, &session.ToolResultMessage{
				ToolCallID: tc.ID, ToolName: tc.Name,
				Content: []session.Content{session.TextContent{Text: "external action boundary is unavailable"}},
				IsError: true, Timestamp: time.Now(),
			}
		}
		actionCtx, cancelAction := contextWithSignal(ctx, signal)
		action, actionErr := cfg.ActionBoundary.PrepareAndAuthorize(actionCtx, ActionRequest{
			ToolName:     tc.Name,
			InvocationID: tc.ID,
			SessionID:    cfg.SessionID,
			TurnID:       cfg.TurnID,
			Arguments:    argsRaw,
			Requirement:  requirement,
			Required:     required,
			CWD:          "",
		})
		cancelAction()
		if actionErr != nil {
			return preparedToolCall{}, &session.ToolResultMessage{
				ToolCallID: tc.ID, ToolName: tc.Name,
				Content: []session.Content{session.TextContent{Text: actionErr.Error()}},
				IsError: true, Timestamp: time.Now(),
			}
		}
		return preparedToolCall{tool: tool, tc: tc, argsRaw: argsRaw, action: action}, nil
	}
	if isCanceled(ctx, signal) {
		return preparedToolCall{}, abortedToolResult(tc)
	}

	return preparedToolCall{tool: tool, tc: tc, argsRaw: argsRaw}, nil
}

func invokeActionDescriptor(
	tool *Tool,
	args json.RawMessage,
) (requirement ApprovalRequirement, required bool, err error) {
	if tool == nil || tool.ApprovalRequirement == nil {
		return ApprovalRequirement{}, false, nil
	}
	defer func() {
		if recovered := recover(); recovered != nil {
			err = fmt.Errorf("action descriptor panic: %v", recovered)
		}
	}()
	return tool.ApprovalRequirement(args)
}

// executeToolCallsParallel executes multiple tool calls concurrently.
// Preparation (tool resolution, validation, hooks) runs sequentially to avoid
// races. Execution runs concurrently.
//
// Reference: Pi agent-loop.js (prepareToolCall sequential, execute concurrent).
func executeToolCallsParallel(
	ctx context.Context,
	snapshot TurnContext,
	assistantMsg session.AssistantMessage,
	toolCalls []*session.ToolCall,
	cfg LoopConfig,
	emit func(session.Event),
	signal <-chan struct{},
) ([]session.ToolResultMessage, bool) {
	type indexedResult struct {
		index  int
		result session.ToolResultMessage
	}

	// Sequential preparation phase: find tools, validate args, run hooks.
	// This is critical for hook safety — BeforeToolCall must not race.
	var prepared []preparedToolCall
	results := make([]session.ToolResultMessage, len(toolCalls))
	eventBuffers := make([][]session.Event, len(toolCalls))
	processed := 0
	for i, tc := range toolCalls {
		argsRaw, _ := json.Marshal(tc.Arguments)
		emit(session.ToolExecStart{ToolCallID: tc.ID, Name: tc.Name, Args: argsRaw})
		p, errResult := prepareToolCall(ctx, snapshot, assistantMsg, tc, cfg, signal)
		if errResult != nil {
			eventBuffers[i] = append(eventBuffers[i], session.ToolExecEnd{ToolCallID: tc.ID, Result: *errResult})
			results[i] = *errResult
		} else {
			p.index = i
			prepared = append(prepared, p)
		}
		processed = i + 1
		if isCanceled(ctx, signal) {
			break
		}
	}

	// Concurrent execution phase. Use a fixed worker pool so a single model
	// response cannot create an unbounded number of goroutines or external
	// effects. Results are indexed and returned in model order.
	workerLimit := cfg.MaxParallelTools
	if workerLimit <= 0 {
		workerLimit = 8
	}
	if workerLimit > len(prepared) {
		workerLimit = len(prepared)
	}
	jobs := make(chan preparedToolCall)
	ch := make(chan indexedResult, len(prepared))
	progressEmitMu := new(sync.Mutex)
	var workers sync.WaitGroup
	workers.Add(workerLimit)
	for i := 0; i < workerLimit; i++ {
		go func() {
			defer workers.Done()
			for p := range jobs {
				progress := newToolProgressGate(emit, progressEmitMu)
				bufferedEmit := func(event session.Event) {
					if _, end := event.(session.ToolExecEnd); end {
						progress.close()
					}
					if _, live := event.(session.ToolExecUpdate); live {
						// Progress is ephemeral and must reach the frontend while
						// the tool is still running. Tool start/end and result
						// ordering remain coordinator-owned below.
						progress.emitUpdate(event)
						return
					}
					eventBuffers[p.index] = append(eventBuffers[p.index], event)
				}
				result := executePreparedToolCall(ctx, snapshot, assistantMsg, p, cfg, bufferedEmit, signal)
				progress.close()
				ch <- indexedResult{p.index, result}
			}
		}()
	}
	for _, p := range prepared {
		jobs <- p
	}
	close(jobs)
	workers.Wait()
	close(ch)
	for r := range ch {
		results[r.index] = r.result
	}
	for i := range eventBuffers {
		for _, event := range eventBuffers[i] {
			emit(event)
		}
	}

	// Pi: terminate when every finalized call has result.terminate === true.
	// Reference: Pi agent-loop.js shouldTerminateToolBatch (line 345).
	results = results[:processed]
	return results, shouldTerminateToolBatch(results)
}

// executePreparedToolCall executes a tool that has already passed preparation.
// The caller emits ToolExecStart before preparation; this emits execution updates and end.
func executePreparedToolCall(
	ctx context.Context,
	snapshot TurnContext,
	assistantMsg session.AssistantMessage,
	p preparedToolCall,
	cfg LoopConfig,
	emit func(session.Event),
	signal <-chan struct{},
) session.ToolResultMessage {
	tc := p.tc
	tool := p.tool
	argsRaw := p.argsRaw
	if isCanceled(ctx, signal) {
		result := *abortedToolResult(tc)
		if p.action != nil && cfg.ActionBoundary != nil {
			_ = cfg.ActionBoundary.Cancel(ctx, p.action, "operation aborted")
		}
		emit(session.ToolExecEnd{ToolCallID: tc.ID, Result: result})
		return result
	}
	// Execute with panic recovery.
	progress := func(p session.ToolPartial) {
		emit(session.ToolExecUpdate{ToolCallID: tc.ID, Partial: p})
	}

	invoke := func(execCtx context.Context, execSignal <-chan struct{}, execProgress func(session.ToolPartial)) (result session.ToolResultMessage, err error) {
		defer func() {
			if r := recover(); r != nil {
				result = session.ToolResultMessage{
					ToolCallID: tc.ID, ToolName: tc.Name,
					Content: []session.Content{session.TextContent{Text: fmt.Sprintf("tool panic: %v", r)}},
					IsError: true, Timestamp: time.Now(),
				}
				err = nil
			}
		}()
		result, err = tool.Execute(execCtx, tc.ID, argsRaw, execSignal, execProgress)
		return result, err
	}

	var result session.ToolResultMessage
	var err error
	if cfg.ActionBoundary != nil {
		result, err = cfg.ActionBoundary.Execute(ctx, p.action, invoke, signal, progress)
	} else {
		result, err = invoke(ctx, signal, progress)
	}
	if err != nil {
		result = session.ToolResultMessage{
			ToolCallID: tc.ID, ToolName: tc.Name,
			Content: []session.Content{session.TextContent{Text: err.Error()}},
			IsError: true, Timestamp: time.Now(),
		}
	}

	// AfterToolCall hook. Errors in the hook produce an error tool result rather
	// than crashing the turn — matching Pi's finalizeExecutedToolCall try/catch.
	// Reference: Pi agent-loop.js finalizeExecutedToolCall (line 450).
	if cfg.AfterToolCall != nil {
		func() {
			defer func() {
				if r := recover(); r != nil {
					result = session.ToolResultMessage{
						ToolCallID: tc.ID,
						ToolName:   tc.Name,
						Content: []session.Content{
							session.TextContent{Text: fmt.Sprintf("afterToolCall hook panic: %v", r)},
						},
						IsError:   true,
						Timestamp: time.Now(),
					}
				}
			}()
			patch := cfg.AfterToolCall(ToolCallResultContext{
				ToolCall: tc, Args: argsRaw, Result: result,
			})
			applyToolCallPatch(&result, patch)
		}()
	}
	emit(session.ToolExecEnd{ToolCallID: tc.ID, Result: result})
	return result
}

func toolResultIdentity(result session.ToolResultMessage) string {
	payload, err := json.Marshal(result)
	if err != nil {
		return ""
	}
	digest := sha256.Sum256(payload)
	return "sha256:" + hex.EncodeToString(digest[:])
}

func firstToolResultText(result session.ToolResultMessage) string {
	for _, content := range result.Content {
		if text, ok := content.(session.TextContent); ok {
			return text.Text
		}
	}
	return "tool returned an error"
}

func applyToolCallPatch(result *session.ToolResultMessage, patch *ToolCallPatch) {
	if patch == nil {
		return
	}
	if patch.Error != nil {
		result.Content = []session.Content{
			session.TextContent{Text: fmt.Sprintf("afterToolCall hook error: %v", patch.Error)},
		}
		result.IsError = true
		return
	}
	if patch.Content != nil {
		result.Content = patch.Content
	}
	if patch.Details != nil {
		result.Details = patch.Details
	}
	if patch.IsError != nil {
		result.IsError = *patch.IsError
	}
	if patch.Terminate != nil {
		result.Terminate = *patch.Terminate
	}
}

func decodeToolArguments(raw string) map[string]any {
	args, err := decodeToolArgumentsStrict(raw)
	if err != nil {
		return map[string]any{}
	}
	return args
}

func decodeToolArgumentsStrict(raw string) (map[string]any, error) {
	args := map[string]any{}
	if strings.TrimSpace(raw) == "" {
		return args, nil
	}
	decoder := json.NewDecoder(strings.NewReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&args); err != nil {
		return nil, err
	}
	if args == nil {
		return nil, errors.New("arguments must be a JSON object")
	}
	var trailing any
	if err := decoder.Decode(&trailing); err != io.EOF {
		if err == nil {
			return nil, errors.New("trailing JSON value")
		}
		return nil, fmt.Errorf("trailing JSON value: %w", err)
	}
	return args, nil
}

func drain(fn func() []session.Message) []session.Message {
	if fn == nil {
		return nil
	}
	return fn()
}

func isAborted(signal <-chan struct{}) bool {
	select {
	case <-signal:
		return true
	default:
		return false
	}
}

func isCanceled(ctx context.Context, signal <-chan struct{}) bool {
	return isAborted(signal) || (ctx != nil && ctx.Err() != nil)
}

// contextWithSignal combines the parent context with the run-local abort
// signal so authorization and provider calls observe the same cancellation
// boundary. The caller owns the returned cancel function.
func contextWithSignal(parent context.Context, signal <-chan struct{}) (context.Context, context.CancelFunc) {
	if parent == nil {
		parent = context.Background()
	}
	if signal == nil {
		return parent, func() {}
	}
	ctx, cancel := context.WithCancel(parent)
	go func() {
		select {
		case <-signal:
			cancel()
		case <-ctx.Done():
		}
	}()
	return ctx, cancel
}

// newFailureMessage is the canonical failure constructor; callers adapt pointer/value.
func newFailureMessage(
	model llm.Model,
	err error,
	aborted bool,
	thinking session.ThinkingLevel,
) session.AssistantMessage {
	stopReason := session.StopReasonError
	if aborted {
		stopReason = session.StopReasonAborted
	}
	return session.AssistantMessage{
		API:           model.API,
		Provider:      model.Provider,
		Model:         model.ID,
		StopReason:    stopReason,
		Error:         err.Error(),
		ThinkingLevel: thinking,
		Timestamp:     time.Now(),
	}
}

func buildPartialMessage(acc llm.StreamAccumulator, model llm.Model) session.AssistantMessage {
	resp := acc.Response()
	msg := session.AssistantMessage{
		API:           resp.API,
		Provider:      resp.Provider,
		Model:         model.ID,
		ResponseModel: resp.ResponseModel,
		ResponseID:    resp.ResponseID,
		StopReason:    session.StopReason(resp.StopReason),
		Timestamp:     time.Now(),
	}
	if msg.API == "" {
		msg.API = model.API
	}
	if msg.Provider == "" {
		msg.Provider = model.Provider
	}
	for _, b := range resp.Blocks {
		switch b := b.(type) {
		case llm.TextBlock:
			msg.Content = append(msg.Content, session.TextContent{Text: b.Text})
		case llm.ThinkingBlock:
			msg.Content = append(msg.Content, session.ThinkingContent{
				Text: b.Thinking, Signature: b.Signature, Redacted: b.Redacted,
			})
		case llm.ToolCallBlock:
			msg.Content = append(
				msg.Content,
				&session.ToolCall{ID: b.ID, Name: b.Name, Arguments: decodeToolArguments(b.Arguments)},
			)
		}
	}
	return msg
}

func buildFailureAssistantMessage(
	acc llm.StreamAccumulator,
	model llm.Model,
	thinking session.ThinkingLevel,
	err error,
) session.AssistantMessage {
	msg := buildPartialMessage(acc, model)
	resp := acc.Response()
	msg.Usage = session.Usage{
		Input:       resp.Usage.InputTokens,
		Output:      resp.Usage.OutputTokens,
		CacheRead:   resp.Usage.CacheReadTokens,
		CacheWrite:  resp.Usage.CacheCreationTokens,
		TotalTokens: resp.Usage.TotalTokens,
		Cost:        session.Cost{Total: resp.Usage.Cost},
	}
	msg.ThinkingLevel = thinking
	msg.StopReason = session.StopReasonError
	msg.Error = err.Error()
	// A partial tool call is not executable evidence. Preserve visible text and
	// thinking for replay, but never persist a malformed call without a result.
	content := msg.Content[:0]
	for _, block := range msg.Content {
		if _, ok := block.(*session.ToolCall); !ok {
			content = append(content, block)
		}
	}
	msg.Content = content
	return msg
}

func buildAssistantMessage(
	acc llm.StreamAccumulator,
	model llm.Model,
	thinking session.ThinkingLevel,
) (session.AssistantMessage, error) {
	resp := acc.Response()
	stopReason := session.StopReason(resp.StopReason)
	responseError := resp.ErrorMessage
	if responseError == "" {
		switch stopReason {
		case session.StopReasonError:
			responseError = "provider response failed"
		case session.StopReasonAborted:
			responseError = "response aborted"
		}
	}
	msg := session.AssistantMessage{
		API:           resp.API,
		Provider:      resp.Provider,
		Model:         model.ID,
		ResponseModel: resp.ResponseModel,
		ResponseID:    resp.ResponseID,
		StopReason:    stopReason,
		Error:         responseError,
		Usage: session.Usage{
			Input:       resp.Usage.InputTokens,
			Output:      resp.Usage.OutputTokens,
			CacheRead:   resp.Usage.CacheReadTokens,
			CacheWrite:  resp.Usage.CacheCreationTokens,
			TotalTokens: resp.Usage.TotalTokens,
			Cost:        session.Cost{Total: resp.Usage.Cost},
		},
		Timestamp:     time.Now(),
		ThinkingLevel: thinking,
	}
	if msg.API == "" {
		msg.API = model.API
	}
	if msg.Provider == "" {
		msg.Provider = model.Provider
	}
	for _, b := range resp.Blocks {
		switch b := b.(type) {
		case llm.TextBlock:
			msg.Content = append(msg.Content, session.TextContent{Text: b.Text})
		case llm.ThinkingBlock:
			msg.Content = append(msg.Content, session.ThinkingContent{
				Text: b.Thinking, Signature: b.Signature, Redacted: b.Redacted,
			})
		case llm.ToolCallBlock:
			args, err := decodeToolArgumentsStrict(b.Arguments)
			if err != nil {
				return session.AssistantMessage{}, fmt.Errorf(
					"tool call %q has invalid arguments: %w", b.Name, err,
				)
			}
			msg.Content = append(
				msg.Content,
				&session.ToolCall{ID: b.ID, Name: b.Name, Arguments: args, Type: b.Type},
			)
		}
	}
	return msg, nil
}
