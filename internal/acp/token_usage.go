package acp

import (
	"bytes"
	"encoding/json"
	"strconv"
	"strings"

	acp "github.com/coder/acp-go-sdk"
)

func tokenUsageFromNotification(n acp.SessionNotification) (input, output int, cost float64, ok bool) {
	metas := []any{n.Meta}
	update := n.Update
	switch {
	case update.UserMessageChunk != nil:
		metas = append(metas, update.UserMessageChunk.Meta)
	case update.AgentMessageChunk != nil:
		metas = append(metas, update.AgentMessageChunk.Meta)
	case update.AgentThoughtChunk != nil:
		metas = append(metas, update.AgentThoughtChunk.Meta)
	case update.ToolCall != nil:
		metas = append(metas, update.ToolCall.Meta)
	case update.ToolCallUpdate != nil:
		metas = append(metas, update.ToolCallUpdate.Meta)
	case update.Plan != nil:
		metas = append(metas, update.Plan.Meta)
	case update.AvailableCommandsUpdate != nil:
		metas = append(metas, update.AvailableCommandsUpdate.Meta)
	case update.CurrentModeUpdate != nil:
		metas = append(metas, update.CurrentModeUpdate.Meta)
	}

	for _, meta := range metas {
		input, output, cost, ok := tokenUsageFromMeta(meta)
		if ok {
			return input, output, cost, true
		}
	}
	return 0, 0, 0, false
}

func tokenUsageFromMeta(meta any) (input, output int, cost float64, ok bool) {
	root, ok := metaMap(meta)
	if !ok {
		return 0, 0, 0, false
	}

	for _, candidate := range usageCandidates(root) {
		input = fieldInt(
			candidate,
			"input", "inputTokens", "input_tokens", "promptTokens", "prompt_tokens",
		)
		output = fieldInt(
			candidate,
			"output", "outputTokens", "output_tokens", "completionTokens", "completion_tokens",
		)
		cost = fieldFloat(candidate, "cost", "costUSD", "cost_usd")
		if input != 0 || output != 0 || cost != 0 {
			return input, output, cost, true
		}
	}
	return 0, 0, 0, false
}

func usageCandidates(root map[string]any) []map[string]any {
	candidates := []map[string]any{root}
	for _, key := range []string{"tokenUsage", "token_usage", "_tokenUsage", "usage"} {
		if nested, ok := metaMap(root[key]); ok {
			candidates = append(candidates, nested)
		}
	}
	return candidates
}

func metaMap(value any) (map[string]any, bool) {
	switch v := value.(type) {
	case nil:
		return nil, false
	case map[string]any:
		return v, true
	case json.RawMessage:
		return decodeMetaMap(v)
	case []byte:
		return decodeMetaMap(v)
	default:
		data, err := json.Marshal(v)
		if err != nil || string(data) == "null" {
			return nil, false
		}
		return decodeMetaMap(data)
	}
}

func decodeMetaMap(data []byte) (map[string]any, bool) {
	var out map[string]any
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := decoder.Decode(&out); err != nil {
		return nil, false
	}
	return out, true
}

func fieldInt(m map[string]any, keys ...string) int {
	for _, key := range keys {
		if value, ok := numberValue(m[key]); ok {
			return int(value)
		}
	}
	return 0
}

func fieldFloat(m map[string]any, keys ...string) float64 {
	for _, key := range keys {
		if value, ok := numberValue(m[key]); ok {
			return value
		}
	}
	return 0
}

func numberValue(value any) (float64, bool) {
	switch v := value.(type) {
	case int:
		return float64(v), true
	case int64:
		return float64(v), true
	case float64:
		return v, true
	case json.Number:
		n, err := v.Float64()
		return n, err == nil
	case string:
		n, err := strconv.ParseFloat(strings.TrimSpace(v), 64)
		return n, err == nil
	default:
		return 0, false
	}
}
