package main

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

func executionModeFor(metadata tool.Metadata) agent.ExecMode {
	if metadata.Concurrency == tool.Parallel {
		return agent.ExecParallel
	}
	return agent.ExecSequential
}

func contextWithToolSignal(ctx context.Context, signal <-chan struct{}) (context.Context, context.CancelFunc) {
	if signal == nil {
		return context.WithCancel(ctx)
	}
	toolCtx, cancel := context.WithCancel(ctx)
	go func() {
		select {
		case <-signal:
			cancel()
		case <-toolCtx.Done():
		}
	}()
	return toolCtx, cancel
}

func executeRegisteredTool(
	ctx context.Context,
	entry tool.ToolEntry,
	id string,
	args json.RawMessage,
	progress func(session.ToolPartial),
) session.ToolResultMessage {
	result := session.ToolResultMessage{
		ToolCallID: id,
		ToolName:   entry.Spec.Name,
		Timestamp:  time.Now(),
	}
	argText := string(args)

	if streaming, ok := entry.Tool.(tool.StreamingUpdateTool); ok {
		var output strings.Builder
		for update, err := range streaming.ExecuteStreamingUpdates(ctx, argText) {
			if err != nil {
				appendToolError(&result, err)
				break
			}
			if update.Snapshot {
				output.Reset()
			}
			output.WriteString(update.Text)
			if progress != nil && update.Text != "" {
				progress(update.Text)
			}
		}
		if output.Len() > 0 {
			result.Content = append(result.Content, session.TextContent{Text: output.String()})
		}
		return result
	}

	if contentTool, ok := entry.Tool.(tool.ContentTool); ok {
		parts, err := contentTool.ExecuteContent(ctx, argText)
		result.Content = sessionContentParts(parts)
		if err != nil {
			appendToolError(&result, err)
		}
		return result
	}

	if detailed, ok := entry.Tool.(tool.ProgressAwareDetailedTool); ok {
		content, details, err := detailed.ExecuteDetailedWithProgress(ctx, argText, func(update tool.StreamUpdate) {
			if progress != nil && update.Text != "" {
				progress(update.Text)
			}
		})
		if content != "" {
			result.Content = append(result.Content, session.TextContent{Text: content})
		}
		if details != nil {
			if raw, ok := details.(json.RawMessage); ok {
				result.Details = append([]byte(nil), raw...)
			} else if data, marshalErr := json.Marshal(details); marshalErr == nil {
				result.Details = data
			} else {
				appendToolError(&result, fmt.Errorf("marshal tool details: %w", marshalErr))
			}
		}
		if err != nil {
			appendToolError(&result, err)
		}
		return result
	}

	if detailed, ok := entry.Tool.(tool.DetailedTool); ok {
		content, details, err := detailed.ExecuteDetailed(ctx, argText)
		if content != "" {
			result.Content = append(result.Content, session.TextContent{Text: content})
		}
		if details != nil {
			if raw, ok := details.(json.RawMessage); ok {
				result.Details = append([]byte(nil), raw...)
			} else if data, marshalErr := json.Marshal(details); marshalErr == nil {
				result.Details = data
			} else {
				appendToolError(&result, fmt.Errorf("marshal tool details: %w", marshalErr))
			}
		}
		if err != nil {
			appendToolError(&result, err)
		}
		return result
	}

	content, err := entry.Tool.Execute(ctx, argText)
	if content != "" {
		result.Content = append(result.Content, session.TextContent{Text: content})
	}
	if err != nil {
		appendToolError(&result, err)
	}
	return result
}

func appendToolError(result *session.ToolResultMessage, err error) {
	if err == nil {
		return
	}
	result.IsError = true
	result.Content = append(result.Content, session.TextContent{Text: err.Error()})
}

func sessionContentParts(parts []llm.ContentPart) []session.Content {
	content := make([]session.Content, 0, len(parts))
	for _, part := range parts {
		switch part.Type {
		case "", llm.ContentPartText:
			if part.Text != "" {
				content = append(content, session.TextContent{Text: part.Text})
			}
		case llm.ContentPartImage:
			if part.Data != "" {
				data, err := base64.StdEncoding.DecodeString(part.Data)
				if err == nil {
					content = append(content, session.ImageContent{Data: data, MimeType: part.MIMEType})
					continue
				}
			}
			if part.URL != "" {
				content = append(content, session.TextContent{Text: "[image: " + part.URL + "]"})
			}
		}
	}
	return content
}
