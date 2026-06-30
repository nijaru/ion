package agent

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

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

	// Outer loop: continues when follow-up messages arrive after agent would stop.
	for {
		hasMoreToolCalls := true

		// Inner loop: process tool calls and steering messages.
		for hasMoreToolCalls || len(pending) > 0 {
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
				emit(session.TurnEnd{Message: *assistantMsg})
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
				results, terminate := executeToolCalls(ctx, currentCtx, *assistantMsg, toolCalls, cfg, emit, signal)
				toolResults = results
				hasMoreToolCalls = !terminate

				for _, result := range toolResults {
					currentCtx.Messages = append(currentCtx.Messages, &result)
					newMessages = append(newMessages, &result)
				}
			}

			emit(session.TurnEnd{Message: assistantMsg, ToolResults: toolResults})

			// Prepare next turn (harness flushes writes, rebuilds context).
			if cfg.PrepareNextTurn != nil {
				snap := cfg.PrepareNextTurn(ctx)
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
		Model:          cfg.Model.ID,
		Messages:       llmMsgs,
		Tools:          tools,
		ReasoningEffort: string(cfg.Thinking),
		SessionID:      "", // set by harness
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

	stream, err := cfg.StreamFn(ctx, req)
	if err != nil {
		return failureMessagePtr(cfg.Model, err, false), true
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
			emit(session.MessageUpdate{Message: &partial, Delta: session.TextDelta{Text: chunk.Content}})
		}
		if chunk.Reasoning != "" {
			emit(session.MessageUpdate{Message: &partial, Delta: session.ThinkingDelta{Text: chunk.Reasoning}})
		}
		for _, call := range chunk.Calls {
			args, _ := json.Marshal(call.Function.Arguments)
			emit(session.MessageUpdate{Message: &partial, Delta: session.ToolCallDelta{
				ToolCallID:     call.ID,
				Name:           call.Function.Name,
				ArgumentsChunk: string(args),
			}})
		}
	}

	if err := stream.Err(); err != nil {
		msg := failureMessage(cfg.Model, err, false)
		if !started {
			emit(session.MessageStart{Message: &msg})
		}
		emit(session.MessageEnd{Message: &msg})
		return &msg, true
	}

	// Build final message from accumulator.
	final := buildAssistantMessage(acc, cfg.Model)
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
	terminate := true

	for _, tc := range toolCalls {
		result := executeOneToolCall(ctx, snapshot, assistantMsg, tc, cfg, emit, signal)
		results = append(results, result)
		if !result.IsError {
			terminate = false
		}
		if isAborted(signal) {
			break
		}
	}

	return results, terminate && len(results) > 0
}

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

	ch := make(chan indexedResult, len(toolCalls))
	for i, tc := range toolCalls {
		go func(idx int, toolCall *session.ToolCall) {
			ch <- indexedResult{idx, executeOneToolCall(ctx, snapshot, assistantMsg, toolCall, cfg, emit, signal)}
		}(i, tc)
	}

	results := make([]session.ToolResultMessage, len(toolCalls))
	terminate := true
	for range toolCalls {
		r := <-ch
		results[r.index] = r.result
		if !r.result.IsError {
			terminate = false
		}
	}

	return results, terminate && len(results) > 0
}

func executeOneToolCall(
	ctx context.Context,
	snapshot TurnContext,
	assistantMsg session.AssistantMessage,
	tc *session.ToolCall,
	cfg LoopConfig,
	emit func(session.Event),
	signal <-chan struct{},
) session.ToolResultMessage {
	emit(session.ToolExecStart{ToolCallID: tc.ID, Name: tc.Name})

	// Find the tool.
	var tool *Tool
	for i := range cfg.Tools {
		if cfg.Tools[i].Name == tc.Name {
			tool = &cfg.Tools[i]
			break
		}
	}

	if tool == nil {
		result := session.ToolResultMessage{
			ToolCallID: tc.ID,
			ToolName:   tc.Name,
			Content:    []session.Content{session.TextContent{Text: "tool not found: " + tc.Name}},
			IsError:    true,
			Timestamp:  time.Now(),
		}
		emit(session.ToolExecEnd{ToolCallID: tc.ID, Result: result})
		return result
	}

	args, _ := json.Marshal(tc.Arguments)

	// BeforeToolCall hook.
	if cfg.BeforeToolCall != nil {
		decision := cfg.BeforeToolCall(ToolCallContext{
			AssistantMessage: assistantMsg,
			ToolCall:         tc,
			Args:             args,
			Context:          snapshot,
		})
		if decision != nil && decision.Block {
			result := session.ToolResultMessage{
				ToolCallID: tc.ID,
				ToolName:   tc.Name,
				Content:    []session.Content{session.TextContent{Text: decision.Reason}},
				IsError:    true,
				Timestamp:  time.Now(),
			}
			emit(session.ToolExecEnd{ToolCallID: tc.ID, Result: result})
			return result
		}
	}

	// Execute with panic recovery.
	progress := func(p session.ToolPartial) {
		emit(session.ToolExecUpdate{ToolCallID: tc.ID, Partial: p})
	}

	var result session.ToolResultMessage
	var err error
	func() {
		defer func() {
			if r := recover(); r != nil {
				result = session.ToolResultMessage{
					ToolCallID: tc.ID,
					ToolName:   tc.Name,
					Content:    []session.Content{session.TextContent{Text: fmt.Sprintf("tool panic: %v", r)}},
					IsError:    true,
					Timestamp:  time.Now(),
				}
			}
		}()
		result, err = tool.Execute(ctx, tc.ID, args, signal, progress)
	}()
	if err != nil {
		result = session.ToolResultMessage{
			ToolCallID: tc.ID,
			ToolName:   tc.Name,
			Content:    []session.Content{session.TextContent{Text: err.Error()}},
			IsError:    true,
			Timestamp:  time.Now(),
		}
	}

	// AfterToolCall hook.
	if cfg.AfterToolCall != nil {
		patch := cfg.AfterToolCall(ToolCallResultContext{
			ToolCall: tc,
			Args:     args,
			Result:   result,
		})
		if patch != nil {
			if patch.Content != nil {
				result.Content = patch.Content
			}
			if patch.Details != nil {
				result.Details = patch.Details
			}
			if patch.IsError != nil {
				result.IsError = *patch.IsError
			}
		}
	}

	emit(session.ToolExecEnd{ToolCallID: tc.ID, Result: result})
	return result
}

// --- helpers ---

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

func failureMessage(model llm.Model, err error, aborted bool) session.AssistantMessage {
	stopReason := session.StopReasonError
	if aborted {
		stopReason = session.StopReasonAborted
	}
	return session.AssistantMessage{
		API:        model.API,
		Provider:   model.Provider,
		Model:      model.ID,
		StopReason: stopReason,
		Error:      err.Error(),
		Timestamp:  time.Now(),
	}
}

func buildPartialMessage(acc llm.StreamAccumulator, model llm.Model) session.AssistantMessage {
	resp := acc.Response()
	msg := session.AssistantMessage{
		Model:      model.ID,
		StopReason: session.StopReason(resp.StopReason),
		Timestamp:  time.Now(),
	}
	for _, b := range resp.Blocks {
		switch b := b.(type) {
		case llm.TextBlock:
			msg.Content = append(msg.Content, session.TextContent{Text: b.Text})
		case llm.ThinkingBlock:
			msg.Content = append(msg.Content, session.ThinkingContent{Text: b.Thinking})
		case llm.ToolCallBlock:
			args := map[string]any{}
			json.Unmarshal([]byte(b.Arguments), &args)
			msg.Content = append(msg.Content, &session.ToolCall{ID: b.ID, Name: b.Name, Arguments: args})
		}
	}
	return msg
}

func buildAssistantMessage(acc llm.StreamAccumulator, model llm.Model) session.AssistantMessage {
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
			Input: resp.Usage.InputTokens,
			Output: resp.Usage.OutputTokens,
			CacheRead: resp.Usage.CacheReadTokens,
			CacheWrite: resp.Usage.CacheCreationTokens,
			TotalTokens: resp.Usage.TotalTokens,
		},
		Timestamp: time.Now(),
	}
	for _, b := range resp.Blocks {
		switch b := b.(type) {
		case llm.TextBlock:
			msg.Content = append(msg.Content, session.TextContent{Text: b.Text})
		case llm.ThinkingBlock:
			msg.Content = append(msg.Content, session.ThinkingContent{Text: b.Thinking})
		case llm.ToolCallBlock:
			args := map[string]any{}
			json.Unmarshal([]byte(b.Arguments), &args)
			msg.Content = append(msg.Content, &session.ToolCall{ID: b.ID, Name: b.Name, Arguments: args})
		}
	}
	return msg
}

func failureMessagePtr(model llm.Model, err error, aborted bool) *session.AssistantMessage {
	msg := failureMessage(model, err, aborted)
	return &msg
}
