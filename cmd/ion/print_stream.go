package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"strings"

	"github.com/nijaru/ion/ctxerr"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

const printEventSchema = "ion.events.v1"

// structuredPrintEvent is the Ion-owned machine stream. It deliberately
// describes runtime behavior rather than exposing the session wire format or
// a provider protocol. Index is local to this command; Sequence is the
// runtime cursor when the host supplied one.
type structuredPrintEvent struct {
	Schema   string `json:"schema"`
	Index    uint64 `json:"index"`
	Sequence uint64 `json:"sequence,omitempty"`
	Type     string `json:"type"`
	Data     any    `json:"data,omitempty"`
}

type structuredPrintWriter struct {
	encoder *json.Encoder
	index   uint64
}

func newStructuredPrintWriter(w io.Writer) *structuredPrintWriter {
	return &structuredPrintWriter{encoder: json.NewEncoder(w)}
}

func (w *structuredPrintWriter) emit(sequence uint64, typ string, data any) error {
	w.index++
	return w.encoder.Encode(structuredPrintEvent{
		Schema:   printEventSchema,
		Index:    w.index,
		Sequence: sequence,
		Type:     typ,
		Data:     data,
	})
}

type printTurnObserver struct {
	result              printResult
	text                strings.Builder
	textWriter          io.Writer
	structured          *structuredPrintWriter
	messageTextStreamed bool
	wroteText           bool
}

func newPrintTurnObserver(runner agent.Runtime, output string, w io.Writer) (*printTurnObserver, error) {
	observer := &printTurnObserver{result: printResult{SessionID: runtimeSessionID(runner)}}
	switch strings.ToLower(strings.TrimSpace(output)) {
	case "", "text":
		observer.textWriter = w
	case "json":
		// JSON output is a final summary and must remain one valid JSON value.
	case "events":
		observer.structured = newStructuredPrintWriter(w)
	default:
		return nil, fmt.Errorf("unsupported print output %q (want text, json, or events)", output)
	}
	return observer, nil
}

func (o *printTurnObserver) observe(envelope agent.EventEnvelope) (bool, error) {
	turnFinished := false
	var turnErr error

	switch msg := envelope.Event.(type) {
	case session.AgentStart:
		if err := o.emit(envelope, "agent_start", map[string]any{
			"session_id": msg.Origin.SessionID,
			"child_id":   msg.Origin.ChildID,
		}); err != nil {
			return false, err
		}
	case session.TurnStart:
		if err := o.emit(envelope, "turn_start", nil); err != nil {
			return false, err
		}
	case session.MessageStart:
		o.messageTextStreamed = false
		if err := o.emit(envelope, "message_start", messageEventData(msg.Message)); err != nil {
			return false, err
		}
	case session.MessageUpdate:
		if msg.BlockType == "text" {
			text := session.DeltaText(msg.Delta)
			o.text.WriteString(text)
			if text != "" {
				o.messageTextStreamed = true
				if err := o.writeText(text); err != nil {
					return false, err
				}
			}
		}
		if err := o.emit(envelope, "message_update", messageUpdateData(msg)); err != nil {
			return false, err
		}
	case session.MessageEnd:
		if assistant, ok := msg.Message.(*session.AssistantMessage); ok {
			if text := session.MessageText(assistant); text != "" {
				o.text.Reset()
				o.text.WriteString(text)
				if !o.messageTextStreamed {
					if err := o.writeText(text); err != nil {
						return false, err
					}
				}
			}
			o.result.InputTokens += assistant.Usage.Input
			o.result.OutputTokens += assistant.Usage.Output
			o.result.Cost += assistant.Usage.Cost.Total
		}
		if err := o.emit(envelope, "message_end", messageEventData(msg.Message)); err != nil {
			return false, err
		}
		o.messageTextStreamed = false
	case session.ToolExecStart:
		o.result.ToolCalls = append(o.result.ToolCalls, msg.Name)
		if err := o.emit(envelope, "tool_start", map[string]any{
			"tool_call_id": msg.ToolCallID,
			"name":         msg.Name,
			"arguments":    jsonValue(msg.Args),
		}); err != nil {
			return false, err
		}
	case session.ToolExecUpdate:
		if err := o.emit(envelope, "tool_update", map[string]any{
			"tool_call_id": msg.ToolCallID,
			"partial":      safeValue(msg.Partial),
		}); err != nil {
			return false, err
		}
	case session.ToolExecEnd:
		if err := o.emit(envelope, "tool_end", map[string]any{
			"tool_call_id": msg.ToolCallID,
			"tool_name":    msg.Result.ToolName,
			"text":         session.MessageText(&msg.Result),
			"is_error":     msg.Result.IsError,
			"details":      jsonValue(msg.Result.Details),
		}); err != nil {
			return false, err
		}
	case session.ApprovalRequest:
		if err := o.emit(envelope, "approval_request", map[string]any{
			"id":           msg.ID,
			"action_id":    msg.ActionID,
			"fingerprint":  msg.Fingerprint,
			"tool_call_id": msg.ToolCallID,
			"tool_name":    msg.ToolName,
			"category":     msg.Category,
			"operation":    msg.Operation,
			"resource":     msg.Resource,
			"cwd":          msg.CWD,
			"paths":        msg.Paths,
		}); err != nil {
			return false, err
		}
	case session.ApprovalResolution:
		if err := o.emit(envelope, "approval_resolution", map[string]any{
			"id":       msg.ID,
			"decision": msg.Decision,
			"reason":   msg.Reason,
		}); err != nil {
			return false, err
		}
	case session.TurnEnd:
		turnFinished = true
		if msg.Message != nil {
			if text := session.MessageText(msg.Message); text != "" {
				o.text.Reset()
				o.text.WriteString(text)
			}
		}
		if msg.Error != nil {
			turnErr = msg.Error
		}
		if err := o.emit(envelope, "turn_end", map[string]any{
			"ok":         msg.Error == nil,
			"response":   session.MessageText(msg.Message),
			"error":      errorText(msg.Error),
			"tool_count": len(msg.ToolResults),
		}); err != nil {
			return false, err
		}
	case session.AgentEnd:
		if err := o.emit(envelope, "agent_end", map[string]any{"message_count": len(msg.Messages)}); err != nil {
			return false, err
		}
	case session.ModelUpdate:
		if err := o.emit(
			envelope,
			"model_update",
			map[string]any{"model": msg.Model, "previous": msg.Previous, "source": msg.Source},
		); err != nil {
			return false, err
		}
	case session.ThinkingUpdate:
		if err := o.emit(
			envelope,
			"thinking_update",
			map[string]any{"level": msg.Level, "previous": msg.Previous},
		); err != nil {
			return false, err
		}
	case session.ToolsUpdate:
		if err := o.emit(
			envelope,
			"tools_update",
			map[string]any{"active": msg.Active, "previous": msg.Previous},
		); err != nil {
			return false, err
		}
	case session.QueueUpdate:
		if err := o.emit(envelope, "queue_update", map[string]any{
			"steer":     messageTexts(msg.Steer),
			"follow_up": messageTexts(msg.FollowUp),
			"next_turn": messageTexts(msg.NextTurn),
		}); err != nil {
			return false, err
		}
	case session.Settled:
		if err := o.emit(envelope, "settled", map[string]any{"next_turn_count": msg.NextTurnCount}); err != nil {
			return false, err
		}
	case session.SavePoint:
		if err := o.emit(
			envelope,
			"save_point",
			map[string]any{"pending_mutations": msg.HadPendingMutations},
		); err != nil {
			return false, err
		}
	case session.Abort:
		if err := o.emit(envelope, "abort", map[string]any{
			"cleared_steer":     messageTexts(msg.ClearedSteer),
			"cleared_follow_up": messageTexts(msg.ClearedFollowUp),
		}); err != nil {
			return false, err
		}
	case session.ProviderRetry:
		if err := o.emit(envelope, "provider_retry", map[string]any{
			"attempt": msg.Attempt,
			"delay":   msg.Delay.String(),
			"error":   errorText(msg.Err),
		}); err != nil {
			return false, err
		}
	case *session.Error:
		if err := o.emit(envelope, "error", map[string]any{"error": errorText(msg.Err)}); err != nil {
			return false, err
		}
	case nil:
		return false, fmt.Errorf("runtime emitted nil event")
	default:
		return false, fmt.Errorf("unsupported runtime event %T", envelope.Event)
	}
	if turnErr != nil {
		return turnFinished, fmt.Errorf("session error: %w", turnErr)
	}
	return turnFinished, nil
}

func (o *printTurnObserver) emit(envelope agent.EventEnvelope, typ string, data any) error {
	if o.structured == nil {
		return nil
	}
	return o.structured.emit(envelope.Sequence, typ, data)
}

func (o *printTurnObserver) emitError(typ string, err error) error {
	if o.structured == nil {
		return nil
	}
	return o.structured.emit(0, "error", map[string]any{"stage": typ, "error": errorText(err)})
}

func (o *printTurnObserver) writeText(text string) error {
	if o.textWriter == nil || text == "" {
		return nil
	}
	if _, err := io.WriteString(o.textWriter, text); err != nil {
		return fmt.Errorf("write text output: %w", err)
	}
	o.wroteText = true
	return nil
}

func (o *printTurnObserver) finish(result printResult, output string, w io.Writer) error {
	switch strings.ToLower(strings.TrimSpace(output)) {
	case "", "text":
		if !o.wroteText {
			if _, err := io.WriteString(w, result.Response); err != nil {
				return err
			}
		}
		_, err := io.WriteString(w, "\n")
		return err
	case "json":
		return writePrintResult(w, result, output)
	case "events":
		return o.structured.emit(0, "result", result)
	default:
		return fmt.Errorf("unsupported print output %q (want text, json, or events)", output)
	}
}

func runPrintModeWithWriter(ctx context.Context, w io.Writer, runner agent.Runtime, prompt, output string) error {
	observer, err := newPrintTurnObserver(runner, output, w)
	if err != nil {
		return err
	}
	result, err := runPromptTurnObserved(ctx, runner, prompt, observer)
	if err != nil {
		return err
	}
	return observer.finish(result, output, w)
}

func runPromptTurn(ctx context.Context, runner agent.Runtime, prompt string) (printResult, error) {
	observer, err := newPrintTurnObserver(runner, "json", io.Discard)
	if err != nil {
		return printResult{}, err
	}
	return runPromptTurnObserved(ctx, runner, prompt, observer)
}

func runPromptTurnObserved(
	ctx context.Context,
	runner agent.Runtime,
	prompt string,
	observer *printTurnObserver,
) (printResult, error) {
	subscription, err := runner.Subscribe(ctx, agent.EventCursor{})
	if err != nil {
		return printResult{}, fmt.Errorf("subscribe runtime events: %w", err)
	}
	defer subscription.Close()

	type promptOutcome struct {
		msg session.Message
		err error
	}
	outcomeCh := make(chan promptOutcome, 1)
	go func() {
		msg, err := runner.Prompt(ctx, prompt)
		outcomeCh <- promptOutcome{msg: msg, err: err}
	}()

	var (
		promptDone   bool
		promptMsg    session.Message
		turnFinished bool
	)
	for !promptDone || !turnFinished {
		select {
		case envelope, ok := <-subscription.Events:
			if !ok {
				if _, _, abortErr := runner.Abort(); abortErr != nil {
					return printResult{}, fmt.Errorf(
						"event stream closed before turn finished: abort turn: %w",
						abortErr,
					)
				}
				_ = observer.emitError("event_stream", fmt.Errorf("event stream closed before turn finished"))
				return printResult{}, fmt.Errorf("event stream closed before turn finished")
			}
			finished, observeErr := observer.observe(envelope)
			turnFinished = turnFinished || finished
			if observeErr != nil {
				_, _, _ = runner.Abort()
				return printResult{}, observeErr
			}
		case outcome := <-outcomeCh:
			promptDone = true
			promptMsg = outcome.msg
			if outcome.err != nil {
				_ = observer.emitError("submit", outcome.err)
				return printResult{}, fmt.Errorf("submit turn: %w", outcome.err)
			}
		case <-ctx.Done():
			_, _, _ = runner.Abort()
			_ = observer.emitError("context", ctx.Err())
			return printResult{}, ctxerr.WrapContext("print turn", ctx.Err())
		}
	}

	result := observer.result
	result.Response = observer.text.String()
	if strings.TrimSpace(result.Response) == "" && promptMsg != nil {
		result.Response = session.MessageText(promptMsg)
	}
	if strings.TrimSpace(result.Response) == "" {
		return printResult{}, fmt.Errorf("turn finished without assistant response")
	}
	result.SessionID = runtimeSessionID(runner)
	return result, nil
}

func messageEventData(msg session.Message) map[string]any {
	return map[string]any{
		"role": messageRole(msg),
		"text": session.MessageText(msg),
	}
}

func messageUpdateData(msg session.MessageUpdate) map[string]any {
	data := map[string]any{"block_type": msg.BlockType}
	switch delta := msg.Delta.(type) {
	case session.TextDelta:
		data["text"] = delta.Text
	case session.ThinkingDelta:
		data["text"] = delta.Text
	case session.ToolCallDelta:
		data["tool_call_id"] = delta.ToolCallID
		data["name"] = delta.Name
		data["arguments_chunk"] = delta.ArgumentsChunk
	default:
		data["value"] = safeValue(delta)
	}
	return data
}

func messageRole(msg session.Message) string {
	switch msg.(type) {
	case *session.UserMessage:
		return "user"
	case *session.AssistantMessage:
		return "assistant"
	case *session.ToolResultMessage:
		return "tool"
	case *session.CustomMessage:
		return "custom"
	default:
		return "unknown"
	}
}

func messageTexts(messages []session.Message) []string {
	if len(messages) == 0 {
		return nil
	}
	texts := make([]string, 0, len(messages))
	for _, msg := range messages {
		texts = append(texts, session.MessageText(msg))
	}
	return texts
}

func errorText(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}

func jsonValue(raw []byte) any {
	if len(raw) == 0 {
		return nil
	}
	var value any
	if err := json.Unmarshal(raw, &value); err != nil {
		return string(raw)
	}
	return value
}

func safeValue(value any) any {
	if value == nil {
		return nil
	}
	if _, err := json.Marshal(value); err == nil {
		return value
	}
	return fmt.Sprint(value)
}
