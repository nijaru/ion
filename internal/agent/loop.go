package agent

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"math/big"
	"reflect"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/google/jsonschema-go/jsonschema"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

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
	convert := cfg.Convert
	if convert == nil {
		convert = DefaultConvert
	}

	newMessages := make([]session.Message, 0, len(prompts))
	currentCtx := TurnContext{
		SystemPrompt: snapshot.SystemPrompt,
		Messages:     append([]session.Message{}, snapshot.Messages...),
	}

	emit(session.AgentStart{Origin: session.SessionOrigin{}})
	emit(session.TurnStart{})

	// Emit prompt messages (Pi: message_start + message_end for each prompt).
	for _, p := range prompts {
		emit(session.MessageStart{Message: p})
		emit(session.MessageEnd{Message: p})
		newMessages = append(newMessages, p)
		currentCtx.Messages = append(currentCtx.Messages, p)
	}

	firstTurn := true

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
				emit(session.MessageStart{Message: &msg})
				emit(session.MessageEnd{Message: &msg})
				emit(session.TurnEnd{Message: msg})
				emit(session.AgentEnd{Messages: newMessages})
				return newMessages
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
			assistantMsg, aborted := streamAssistantResponse(ctx, currentCtx, cfg, convert, emit, signal)
			newMessages = append(newMessages, assistantMsg)
			currentCtx.Messages = append(currentCtx.Messages, assistantMsg)

			if aborted {
				// Terminal: emit turn_end + agent_end and return.
				emit(session.TurnEnd{Message: assistantMsg})
				emit(session.AgentEnd{Messages: newMessages})
				return newMessages
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
					results, terminate := executeToolCalls(ctx, currentCtx, *assistantMsg, toolCalls, cfg, emit, signal)
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
				snap := cfg.PrepareNextTurn(ctx, toolResults)
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
				return newMessages
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
	return newMessages
}

func isContextOverflow(cfg LoopConfig, err error) bool {
	if err == nil {
		return false
	}
	if cfg.ContextOverflow != nil && cfg.ContextOverflow(err) {
		return true
	}
	return IsContextOverflowError(err)
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
) (*session.AssistantMessage, bool) {
	if isCanceled(ctx, signal) {
		msg := newFailureMessage(cfg.Model, context.Canceled, true, cfg.Thinking)
		msg.Error = "response aborted"
		emit(session.MessageStart{Message: &msg})
		emit(session.MessageEnd{Message: &msg})
		return &msg, true
	}

	// Derive a cancellable context from the signal channel so the
	// provider stream is cancelled when the run is aborted.
	streamCtx, cancelStream := contextWithSignal(ctx, signal)
	defer cancelStream()

	msgs := snapshot.Messages
	if cfg.TransformCtx != nil {
		msgs = cfg.TransformCtx(ctx, msgs)
	}

	llmMsgs := convert(msgs)

	// Prepend system prompt as a system message if present.
	// Pi stores systemPrompt in the context object; the model resolver layer
	// injects it. Ion builds requests directly, so prepend here.
	if snapshot.SystemPrompt != "" {
		sysMsg := llm.Message{
			Role:    llm.RoleSystem,
			Content: snapshot.SystemPrompt,
		}
		llmMsgs = append([]llm.Message{sysMsg}, llmMsgs...)
	}
	tools := make([]*llm.Spec, len(cfg.Tools))
	for i, t := range cfg.Tools {
		tools[i] = &llm.Spec{
			Name:        t.Name,
			Description: t.Description,
			Parameters:  t.Parameters,
		}
	}

	req := &llm.Request{
		Model:           cfg.Model.ID,
		Messages:        llmMsgs,
		Tools:           tools,
		ReasoningEffort: providerReasoningEffort(cfg.Thinking),
		SessionID:       cfg.SessionID,
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
		return &msg, true
	}

	stream, err := cfg.StreamFn(streamCtx, req)
	if err == nil && stream == nil {
		err = fmt.Errorf("provider returned a nil stream")
	}
	if err != nil {
		if isContextOverflow(cfg, err) {
			err = fmt.Errorf("context_length_exceeded: %w", err)
		}
		msg := newFailureMessage(cfg.Model, err, false, cfg.Thinking)
		emit(session.MessageStart{Message: &msg})
		emit(session.MessageEnd{Message: &msg})
		return &msg, true
	}
	defer stream.Close()

	var acc llm.StreamAccumulator
	started := false

	for {
		chunk, ok := stream.Next()
		if !ok {
			break
		}

		acc.Add(chunk)

		// Build partial message from accumulator state.
		partial := buildPartialMessage(acc, cfg.Model)
		if !started {
			emit(session.MessageStart{Message: &partial})
			started = true
		}

		// Emit delta events.
		if chunk.Content != "" {
			emit(session.MessageUpdate{
				Message:   &partial,
				Delta:     session.TextDelta{Text: chunk.Content},
				BlockType: "text",
			})
		}
		if chunk.Reasoning != "" {
			emit(session.MessageUpdate{
				Message:   &partial,
				Delta:     session.ThinkingDelta{Text: chunk.Reasoning},
				BlockType: "thinking",
			})
		}
		for _, call := range chunk.Calls {
			args, _ := json.Marshal(call.Function.Arguments)
			emit(session.MessageUpdate{
				Message: &partial,
				Delta: session.ToolCallDelta{
					ToolCallID:     call.ID,
					Name:           call.Function.Name,
					ArgumentsChunk: string(args),
				},
				BlockType: "tool_call",
			})
		}
	}

	if isCanceled(ctx, signal) {
		// Cancellation may surface as ok=false with a nil stream error;
		// treat it as an aborted turn, not a completed one. Pi agent-loop.js abort branch.
		final := buildAssistantMessage(acc, cfg.Model, cfg.Thinking)
		final.StopReason = session.StopReasonAborted
		final.Error = "response aborted"
		if !started {
			emit(session.MessageStart{Message: &final})
		}
		emit(session.MessageEnd{Message: &final})
		return &final, true
	}
	if err := stream.Err(); err != nil {
		if isContextOverflow(cfg, err) {
			err = fmt.Errorf("context_length_exceeded: %w", err)
		}
		msg := newFailureMessage(cfg.Model, err, false, cfg.Thinking)
		if !started {
			emit(session.MessageStart{Message: &msg})
		}
		emit(session.MessageEnd{Message: &msg})
		return &msg, true
	}

	// Build final message from accumulator.
	final := buildAssistantMessage(acc, cfg.Model, cfg.Thinking)
	if !started {
		emit(session.MessageStart{Message: &final})
	}
	emit(session.MessageEnd{Message: &final})
	return &final, false
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

	for _, tc := range toolCalls {
		argsRaw, _ := json.Marshal(tc.Arguments)
		emit(session.ToolExecStart{ToolCallID: tc.ID, Name: tc.Name, Args: argsRaw})
		prepared, preparationResult := prepareToolCall(ctx, snapshot, assistantMsg, tc, cfg, signal)
		var result session.ToolResultMessage
		if preparationResult != nil {
			result = *preparationResult
			emit(session.ToolExecEnd{ToolCallID: tc.ID, Result: result})
		} else {
			result = executePreparedToolCall(ctx, snapshot, assistantMsg, prepared, cfg, emit, signal)
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
	processed := 0
	for i, tc := range toolCalls {
		argsRaw, _ := json.Marshal(tc.Arguments)
		emit(session.ToolExecStart{ToolCallID: tc.ID, Name: tc.Name, Args: argsRaw})
		p, errResult := prepareToolCall(ctx, snapshot, assistantMsg, tc, cfg, signal)
		if errResult != nil {
			emit(session.ToolExecEnd{ToolCallID: tc.ID, Result: *errResult})
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
	eventBuffers := make([][]session.Event, len(toolCalls))
	var workers sync.WaitGroup
	workers.Add(workerLimit)
	for i := 0; i < workerLimit; i++ {
		go func() {
			defer workers.Done()
			for p := range jobs {
				bufferedEmit := func(event session.Event) {
					eventBuffers[p.index] = append(eventBuffers[p.index], event)
				}
				ch <- indexedResult{p.index, executePreparedToolCall(ctx, snapshot, assistantMsg, p, cfg, bufferedEmit, signal)}
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
	for _, p := range prepared {
		for _, event := range eventBuffers[p.index] {
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

// validateArgs validates the JSON Schema used by a tool before execution.
// Tool specs arrive from both native Go values and JSON-encoded schemas, so the
// boundary normalizes both forms before recursively checking required fields,
// nested objects/arrays, primitive types, enums, and object closure.
func validateArgs(params any, args map[string]any) error {
	_, err := coerceAndValidateArgs(params, args)
	return err
}

func coerceAndValidateArgs(params any, args map[string]any) (map[string]any, error) {
	schema, ok, err := normalizeSchemaMap(params)
	if err != nil || !ok {
		return args, err
	}
	data, err := json.Marshal(args)
	if err != nil {
		return args, fmt.Errorf("invalid tool arguments: %w", err)
	}
	var normalized map[string]any
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&normalized); err != nil {
		return args, fmt.Errorf("invalid tool arguments: %w", err)
	}
	if coerced, ok := coerceSchemaValue(schema, normalized).(map[string]any); ok {
		normalized = coerced
	}
	if err := validateExactNumericSchema(schema, normalized, "root"); err != nil {
		return args, err
	}
	if err := validateJSONSchema(schema, normalized); err != nil {
		return args, err
	}
	return normalized, nil
}

func exactRational(value any) (*big.Rat, bool) {
	switch number := value.(type) {
	case json.Number:
		rational, valid := new(big.Rat).SetString(number.String())
		return rational, valid
	case float64:
		if math.IsNaN(number) || math.IsInf(number, 0) {
			return nil, false
		}
		rational, valid := new(big.Rat).SetString(strconv.FormatFloat(number, 'g', -1, 64))
		return rational, valid
	case float32:
		return exactRational(float64(number))
	case int:
		return new(big.Rat).SetInt64(int64(number)), true
	case int8:
		return new(big.Rat).SetInt64(int64(number)), true
	case int16:
		return new(big.Rat).SetInt64(int64(number)), true
	case int32:
		return new(big.Rat).SetInt64(int64(number)), true
	case int64:
		return new(big.Rat).SetInt64(number), true
	case uint:
		return new(big.Rat).SetUint64(uint64(number)), true
	case uint8:
		return new(big.Rat).SetUint64(uint64(number)), true
	case uint16:
		return new(big.Rat).SetUint64(uint64(number)), true
	case uint32:
		return new(big.Rat).SetUint64(uint64(number)), true
	case uint64:
		return new(big.Rat).SetUint64(number), true
	case uintptr:
		return new(big.Rat).SetUint64(uint64(number)), true
	default:
		return nil, false
	}
}

func validateExactNumericSchema(schema map[string]any, instance any, path string) error {
	if boolean, marked := schema[booleanSchemaMarker].(bool); marked {
		if !boolean {
			return fmt.Errorf("%s is disallowed by schema", path)
		}
		return nil
	}
	if alternatives := schemaSchemas(schema["allOf"]); alternatives != nil {
		for _, alternative := range alternatives {
			if err := validateExactNumericSchema(alternative, instance, path); err != nil {
				return err
			}
		}
	}
	for _, keyword := range []string{"anyOf", "oneOf"} {
		if alternatives := schemaSchemas(schema[keyword]); alternatives != nil {
			var firstErr error
			for _, alternative := range alternatives {
				if err := validateExactNumericSchema(alternative, instance, path); err == nil {
					firstErr = nil
					break
				} else if firstErr == nil {
					firstErr = err
				}
			}
			if firstErr != nil {
				return firstErr
			}
		}
	}
	types := schemaTypes(schema["type"])
	hasNumberType := false
	for _, typ := range types {
		if typ == "number" {
			hasNumberType = true
			break
		}
	}
	if number, ok := instance.(json.Number); ok && len(types) > 0 {
		matched := false
		for _, typ := range types {
			if schemaTypeMatches(typ, number) {
				matched = true
				break
			}
		}
		if !matched {
			return fmt.Errorf("%s has an incompatible type", path)
		}
	}
	for _, typ := range types {
		if number, ok := instance.(json.Number); ok {
			if typ == "integer" {
				rational, valid := new(big.Rat).SetString(number.String())
				if (!valid || !rational.IsInt()) && !hasNumberType {
					return fmt.Errorf("%s must be an integer", path)
				}
			}
			if typ == "number" {
				parsed, err := number.Float64()
				if err != nil || math.IsNaN(parsed) || math.IsInf(parsed, 0) {
					return fmt.Errorf("%s must be a finite number", path)
				}
			}
		}
	}
	if number, ok := exactRational(instance); ok {
		if expected, exists := schema["const"]; exists {
			if expectedNumber, numeric := exactRational(expected); numeric {
				if number.Cmp(expectedNumber) != 0 {
					return fmt.Errorf("%s does not equal const", path)
				}
			}
		}
		if values, ok := schema["enum"].([]any); ok {
			matched := false
			for _, value := range values {
				if expectedNumber, numeric := exactRational(value); numeric && number.Cmp(expectedNumber) == 0 {
					matched = true
					break
				}
			}
			if !matched {
				return fmt.Errorf("%s is not in enum", path)
			}
		}
		if minimum, valid := exactRational(schema["minimum"]); valid && number.Cmp(minimum) < 0 {
			return fmt.Errorf("%s is below minimum", path)
		}
		exclusiveMinimum := schema["exclusiveMinimum"]
		if enabled, ok := exclusiveMinimum.(bool); ok && enabled {
			exclusiveMinimum = schema["minimum"]
		}
		if minimum, valid := exactRational(exclusiveMinimum); valid && number.Cmp(minimum) <= 0 {
			return fmt.Errorf("%s is not above exclusive minimum", path)
		}
		if maximum, valid := exactRational(schema["maximum"]); valid && number.Cmp(maximum) > 0 {
			return fmt.Errorf("%s exceeds maximum", path)
		}
		exclusiveMaximum := schema["exclusiveMaximum"]
		if enabled, ok := exclusiveMaximum.(bool); ok && enabled {
			exclusiveMaximum = schema["maximum"]
		}
		if maximum, valid := exactRational(exclusiveMaximum); valid && number.Cmp(maximum) >= 0 {
			return fmt.Errorf("%s is not below exclusive maximum", path)
		}
		if multiple, valid := exactRational(schema["multipleOf"]); valid && multiple.Sign() > 0 {
			quotient := new(big.Rat).Quo(number, multiple)
			if !quotient.IsInt() {
				return fmt.Errorf("%s is not a multiple of %v", path, multiple)
			}
		}
	}
	if object, ok := schemaObject(instance); ok {
		properties, err := schemaPropertyValues(schema["properties"])
		if err != nil {
			return err
		}
		for name, rawProperty := range properties {
			value, exists := object[name]
			if !exists {
				continue
			}
			if boolean, isBooleanSchema := rawProperty.(bool); isBooleanSchema {
				if !boolean {
					return fmt.Errorf("%s is disallowed by schema", joinValidationPath(path, name))
				}
				continue
			}
			property, valid, err := normalizeSchemaMap(rawProperty)
			if err != nil {
				return err
			}
			if !valid {
				return fmt.Errorf("property %q is not a schema", name)
			}
			if err := validateExactNumericSchema(property, value, joinValidationPath(path, name)); err != nil {
				return err
			}
		}
		additional := schema["additionalProperties"]
		additionalSchema, additionalValid, err := normalizeSchemaMapValue(additional)
		if err != nil && additional != nil {
			return err
		}
		patterns, err := schemaPropertyValues(schema["patternProperties"])
		if err != nil {
			return err
		}
		for name, value := range object {
			matchedPattern := false
			for pattern, rawPattern := range patterns {
				matched, matchErr := regexp.MatchString(pattern, name)
				if matchErr != nil {
					return matchErr
				}
				if !matched {
					continue
				}
				matchedPattern = true
				if boolean, isBooleanSchema := rawPattern.(bool); isBooleanSchema {
					if !boolean {
						return fmt.Errorf("%s is disallowed by schema", joinValidationPath(path, name))
					}
					continue
				}
				patternSchema, valid, normalizeErr := normalizeSchemaMap(rawPattern)
				if normalizeErr != nil {
					return normalizeErr
				}
				if valid {
					if err := validateExactNumericSchema(
						patternSchema,
						value,
						joinValidationPath(path, name),
					); err != nil {
						return err
					}
				}
			}
			if matchedPattern {
				continue
			}
			if _, defined := properties[name]; defined {
				continue
			}
			if boolean, isBooleanSchema := additional.(bool); isBooleanSchema {
				if !boolean {
					return fmt.Errorf("%s is disallowed by schema", joinValidationPath(path, name))
				}
			} else if additionalValid {
				if err := validateExactNumericSchema(
					additionalSchema,
					value,
					joinValidationPath(path, name),
				); err != nil {
					return err
				}
			}
		}
	}
	if array, ok := schemaArray(instance); ok {
		if tuple, valid := schema["items"].([]any); valid {
			for i, value := range array {
				if i >= len(tuple) {
					break
				}
				if boolean, isBooleanSchema := tuple[i].(bool); isBooleanSchema {
					if !boolean {
						return fmt.Errorf("%s[%d] is disallowed by schema", path, i)
					}
					continue
				}
				items, valid, err := normalizeSchemaMap(tuple[i])
				if err != nil {
					return err
				}
				if valid {
					if err := validateExactNumericSchema(items, value, fmt.Sprintf("%s[%d]", path, i)); err != nil {
						return err
					}
				}
			}
		} else if items, valid, err := normalizeSchemaMapValue(schema["items"]); err != nil {
			return err
		} else if valid {
			for i, value := range array {
				if err := validateExactNumericSchema(items, value, fmt.Sprintf("%s[%d]", path, i)); err != nil {
					return err
				}
			}
		}
		if prefix, valid := schema["prefixItems"].([]any); valid {
			for i, value := range array {
				if i >= len(prefix) {
					break
				}
				if boolean, isBooleanSchema := prefix[i].(bool); isBooleanSchema {
					if !boolean {
						return fmt.Errorf("%s[%d] is disallowed by schema", path, i)
					}
					continue
				}
				items, valid, err := normalizeSchemaMap(prefix[i])
				if err != nil {
					return err
				}
				if valid {
					if err := validateExactNumericSchema(items, value, fmt.Sprintf("%s[%d]", path, i)); err != nil {
						return err
					}
				}
			}
		}
	}

	return nil
}

func joinValidationPath(path, name string) string {
	if path == "root" {
		return name
	}
	return path + "." + name
}

func validateSchemaValue(schema map[string]any, instance any) error {
	if err := validateExactNumericSchema(schema, instance, "root"); err != nil {
		return err
	}
	return validateJSONSchema(schema, instance)
}

func validateJSONSchema(value any, instance any) error {
	data, err := json.Marshal(normalizeSchemaForValidator(value))
	if err != nil {
		return fmt.Errorf("invalid tool schema: %w", err)
	}
	var schema jsonschema.Schema
	if err := json.Unmarshal(data, &schema); err != nil {
		return fmt.Errorf("invalid tool schema: %w", err)
	}
	resolved, err := schema.Resolve(nil)
	if err != nil {
		return fmt.Errorf("invalid tool schema: %w", err)
	}
	return resolved.Validate(normalizeNumbersForValidation(cloneSchemaValue(instance)))
}

func normalizeSchemaForValidator(value any) any {
	switch value := value.(type) {
	case map[string]any:
		if boolean, marked := value[booleanSchemaMarker]; marked && len(value) == 1 {
			return normalizeSchemaForValidator(boolean)
		}
		result := make(map[string]any, len(value))
		for key, nested := range value {
			if key == "minimum" || key == "maximum" || key == "exclusiveMinimum" || key == "exclusiveMaximum" ||
				key == "multipleOf" {
				continue
			}
			result[key] = normalizeSchemaForValidator(nested)
		}
		if tuple, ok := value["items"].([]any); ok {
			delete(result, "items")
			result["prefixItems"] = normalizeSchemaForValidator(tuple)
			if additional, exists := value["additionalItems"]; exists {
				result["items"] = normalizeSchemaForValidator(additional)
			}
		}
		return result
	case []any:
		result := make([]any, len(value))
		for i, nested := range value {
			result[i] = normalizeSchemaForValidator(nested)
		}
		return result
	case bool:
		if value {
			return map[string]any{}
		}
		return map[string]any{"not": map[string]any{}}
	default:
		return value
	}
}

func normalizeNumbersForValidation(value any) any {
	switch value := value.(type) {
	case json.Number:
		if number, err := value.Float64(); err == nil {
			return number
		}
	case map[string]any:
		for key, nested := range value {
			value[key] = normalizeNumbersForValidation(nested)
		}
	case []any:
		for i, nested := range value {
			value[i] = normalizeNumbersForValidation(nested)
		}
	}
	return value
}

func coerceSchemaValue(schema map[string]any, value any) any {
	if alternatives := schemaSchemas(schema["allOf"]); alternatives != nil {
		for _, alternative := range alternatives {
			value = coerceSchemaValue(alternative, value)
		}
	}
	if alternatives := schemaAlternativeValues(schema["anyOf"]); alternatives != nil {
		value = coerceSchemaUnion(value, alternatives)
	}
	if alternatives := schemaAlternativeValues(schema["oneOf"]); alternatives != nil {
		value = coerceSchemaUnion(value, alternatives)
	}

	types := schemaTypes(schema["type"])
	matches := false
	for _, typ := range types {
		if schemaTypeMatches(typ, value) {
			matches = true
			break
		}
	}
	if len(types) > 0 && !matches {
		for _, typ := range types {
			if converted, ok := coercePrimitiveByType(value, typ); ok {
				value = converted
				break
			}
		}
	}
	if object, ok := schemaObject(value); ok {
		properties, _ := schemaPropertyValues(schema["properties"])
		for name, rawProperty := range properties {
			if current, exists := object[name]; exists {
				if property, valid, _ := normalizeSchemaMap(rawProperty); valid {
					object[name] = coerceSchemaValue(property, current)
				}
			}
		}
		if additional, valid, _ := normalizeSchemaMapValue(schema["additionalProperties"]); valid {
			for name, current := range object {
				if _, defined := properties[name]; !defined {
					object[name] = coerceSchemaValue(additional, current)
				}
			}
		}
	}
	if array, ok := schemaArray(value); ok {
		if tuple, valid := schema["items"].([]any); valid {
			for i, item := range array {
				if i < len(tuple) {
					if itemSchema, valid, _ := normalizeSchemaMap(tuple[i]); valid {
						array[i] = coerceSchemaValue(itemSchema, item)
					}
				}
			}
		} else if items, valid, _ := normalizeSchemaMapValue(schema["items"]); valid {
			for i, item := range array {
				array[i] = coerceSchemaValue(items, item)
			}
		}
	}
	return value
}

func coerceSchemaUnion(value any, alternatives []any) any {
	for _, rawAlternative := range alternatives {
		alternative, valid, err := normalizeSchemaMap(rawAlternative)
		if err != nil || !valid {
			if boolean, ok := rawAlternative.(bool); ok && boolean {
				return value
			}
			continue
		}
		candidate := cloneSchemaValue(value)
		candidate = coerceSchemaValue(alternative, candidate)
		if validateSchemaValue(alternative, candidate) == nil {
			return candidate
		}
	}
	return value
}

func cloneSchemaValue(value any) any {
	data, err := json.Marshal(value)
	if err != nil {
		return value
	}
	var clone any
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&clone); err != nil {
		return value
	}
	return clone
}

func coercePrimitiveByType(value any, typ string) (any, bool) {
	switch typ {
	case "number":
		if value == nil {
			return float64(0), true
		}
		if text, ok := value.(string); ok && strings.TrimSpace(text) != "" {
			if number, err := strconv.ParseFloat(
				text,
				64,
			); err == nil && !math.IsNaN(number) &&
				!math.IsInf(number, 0) {
				return number, true
			}
		}
		if boolean, ok := value.(bool); ok {
			if boolean {
				return float64(1), true
			}
			return float64(0), true
		}
	case "integer":
		if value == nil {
			return float64(0), true
		}
		if text, ok := value.(string); ok && strings.TrimSpace(text) != "" {
			if number, err := strconv.ParseFloat(
				text,
				64,
			); err == nil && math.Trunc(number) == number &&
				!math.IsInf(number, 0) {
				return number, true
			}
		}
		if boolean, ok := value.(bool); ok {
			if boolean {
				return float64(1), true
			}
			return float64(0), true
		}
	case "boolean":
		if value == nil {
			return false, true
		}
		if text, ok := value.(string); ok {
			if text == "true" {
				return true, true
			}
			if text == "false" {
				return false, true
			}
		}
		if number, ok := schemaNumber(value); ok {
			if number == 1 {
				return true, true
			}
			if number == 0 {
				return false, true
			}
		}
	case "string":
		if value == nil {
			return "", true
		}
		switch converted := value.(type) {
		case bool:
			return strconv.FormatBool(converted), true
		case float64:
			return strconv.FormatFloat(converted, 'f', -1, 64), true
		case json.Number:
			return converted.String(), true
		}
	case "null":
		if value == "" || value == false {
			return nil, true
		}
		if number, ok := schemaNumber(value); ok && number == 0 {
			return nil, true
		}
	}
	return value, false
}

func normalizeSchemaMap(value any) (map[string]any, bool, error) {
	if value == nil {
		return nil, false, nil
	}
	if schema, ok := value.(map[string]any); ok {
		return schema, true, nil
	}
	var data []byte
	switch schema := value.(type) {
	case string:
		data = []byte(schema)
	case json.RawMessage:
		data = []byte(schema)
	default:
		encoded, err := json.Marshal(value)
		if err != nil {
			return nil, false, fmt.Errorf("invalid tool schema: %w", err)
		}
		data = encoded
	}
	var schema map[string]any
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&schema); err != nil {
		return nil, false, fmt.Errorf("invalid tool schema: %w", err)
	}
	if schema == nil {
		return nil, false, nil
	}
	return schema, true, nil
}

func schemaTypes(value any) []string {
	if text, ok := value.(string); ok {
		return []string{text}
	}
	return schemaStrings(value)
}

func schemaStrings(value any) []string {
	var result []string
	switch values := value.(type) {
	case []string:
		return append(result, values...)
	case []any:
		for _, value := range values {
			if text, ok := value.(string); ok {
				result = append(result, text)
			}
		}
	}
	return result
}

func schemaTypeMatches(typ string, value any) bool {
	switch typ {
	case "string":
		_, ok := value.(string)
		return ok
	case "number":
		_, ok := schemaNumber(value)
		return ok
	case "integer":
		if number, ok := value.(json.Number); ok {
			rational, valid := exactRational(number)
			return valid && rational.IsInt()
		}
		v := reflect.ValueOf(value)
		if !v.IsValid() {
			return false
		}
		switch v.Kind() {
		case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64,
			reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
			return true
		case reflect.Float32, reflect.Float64:
			return math.Trunc(v.Float()) == v.Float()
		default:
			return false
		}
	case "boolean":
		_, ok := value.(bool)
		return ok
	case "array":
		_, ok := schemaArray(value)
		return ok
	case "object":
		_, ok := schemaObject(value)
		return ok
	case "null":
		return value == nil
	default:
		return false
	}
}

func schemaNumber(value any) (float64, bool) {
	if number, ok := value.(json.Number); ok {
		parsed, err := number.Float64()
		return parsed, err == nil && !math.IsNaN(parsed) && !math.IsInf(parsed, 0)
	}
	v := reflect.ValueOf(value)
	if !v.IsValid() {
		return 0, false
	}
	switch v.Kind() {
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64:
		return float64(v.Int()), true
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64, reflect.Uintptr:
		return float64(v.Uint()), true
	case reflect.Float32, reflect.Float64:
		parsed := v.Float()
		return parsed, !math.IsNaN(parsed) && !math.IsInf(parsed, 0)
	default:
		return 0, false
	}
}

func schemaObject(value any) (map[string]any, bool) {
	object, ok := value.(map[string]any)
	return object, ok
}

func schemaArray(value any) ([]any, bool) {
	if array, ok := value.([]any); ok {
		return array, true
	}
	v := reflect.ValueOf(value)
	if !v.IsValid() || (v.Kind() != reflect.Array && v.Kind() != reflect.Slice) {
		return nil, false
	}
	array := make([]any, v.Len())
	for i := range array {
		array[i] = v.Index(i).Interface()
	}
	return array, true
}

func schemaPropertyValues(value any) (map[string]any, error) {
	if value == nil {
		return nil, nil
	}
	if properties, ok := value.(map[string]any); ok {
		return properties, nil
	}
	data, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}
	properties := map[string]any{}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&properties); err != nil {
		return nil, err
	}
	return properties, nil
}

func normalizeSchemaMapValue(value any) (map[string]any, bool, error) {
	if _, boolean := value.(bool); boolean {
		return nil, false, nil
	}
	schema, ok, err := normalizeSchemaMap(value)
	return schema, ok, err
}

const booleanSchemaMarker = "__ion_boolean_schema"

func schemaAlternativeValues(value any) []any {
	values, ok := schemaArray(value)
	if !ok {
		return nil
	}
	return values
}

func schemaSchemas(value any) []map[string]any {
	values := schemaAlternativeValues(value)
	if values == nil {
		return nil
	}
	result := make([]map[string]any, 0, len(values))
	for _, value := range values {
		if boolean, ok := value.(bool); ok {
			result = append(result, map[string]any{booleanSchemaMarker: boolean})
			continue
		}
		schema, valid, err := normalizeSchemaMap(value)
		if err != nil || !valid {
			return nil
		}
		result = append(result, schema)
	}
	return result
}

// --- helpers ---

func decodeToolArguments(raw string) map[string]any {
	args := map[string]any{}
	decoder := json.NewDecoder(strings.NewReader(raw))
	decoder.UseNumber()
	if err := decoder.Decode(&args); err != nil {
		return map[string]any{}
	}
	return args
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
			msg.Content = append(msg.Content, session.ThinkingContent{Text: b.Thinking})
		case llm.ToolCallBlock:
			msg.Content = append(
				msg.Content,
				&session.ToolCall{ID: b.ID, Name: b.Name, Arguments: decodeToolArguments(b.Arguments)},
			)
		}
	}
	return msg
}

func buildAssistantMessage(
	acc llm.StreamAccumulator,
	model llm.Model,
	thinking session.ThinkingLevel,
) session.AssistantMessage {
	resp := acc.Response()
	msg := session.AssistantMessage{
		API:           resp.API,
		Provider:      resp.Provider,
		Model:         model.ID,
		ResponseModel: resp.ResponseModel,
		ResponseID:    resp.ResponseID,
		StopReason:    session.StopReason(resp.StopReason),
		Error:         resp.ErrorMessage,
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
			msg.Content = append(msg.Content, session.ThinkingContent{Text: b.Thinking})
		case llm.ToolCallBlock:
			msg.Content = append(
				msg.Content,
				&session.ToolCall{ID: b.ID, Name: b.Name, Arguments: decodeToolArguments(b.Arguments)},
			)
		}
	}
	return msg
}
