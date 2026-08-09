package openai

import (
	"bytes"
	stdjson "encoding/json"
	"io"
	"net/http"
)

// reasoningContentTransport repairs the pinned go-openai SDK's omission of an
// empty reasoning_content field. Some OpenAI-compatible reasoning endpoints
// require the field on every replayed assistant message, including messages
// with no locally retained reasoning text. This stays at the wire boundary so
// the neutral request never contains provider-specific sentinel content.
type reasoningContentTransport struct {
	base http.RoundTripper
}

func (t reasoningContentTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	if req.Body == nil {
		return t.roundTrip(req)
	}

	body, err := io.ReadAll(req.Body)
	_ = req.Body.Close()
	if err != nil {
		return nil, err
	}

	repaired, changed := addMissingReasoningContent(body)
	if changed {
		body = repaired
	}
	req.Body = io.NopCloser(bytes.NewReader(body))
	req.ContentLength = int64(len(body))
	return t.roundTrip(req)
}

func (t reasoningContentTransport) roundTrip(req *http.Request) (*http.Response, error) {
	base := t.base
	if base == nil {
		base = http.DefaultTransport
	}
	return base.RoundTrip(req)
}

func addMissingReasoningContent(body []byte) ([]byte, bool) {
	var envelope map[string]stdjson.RawMessage
	if err := stdjson.Unmarshal(body, &envelope); err != nil {
		return body, false
	}
	messageRaw, ok := envelope["messages"]
	if !ok {
		return body, false
	}

	var messages []map[string]stdjson.RawMessage
	if err := stdjson.Unmarshal(messageRaw, &messages); err != nil {
		return body, false
	}

	changed := false
	for _, message := range messages {
		role, ok := message["role"]
		if !ok || string(role) != `"assistant"` {
			continue
		}
		if _, exists := message["reasoning_content"]; exists {
			continue
		}
		message["reasoning_content"] = stdjson.RawMessage(`""`)
		changed = true
	}
	if !changed {
		return body, false
	}

	messageRaw, err := stdjson.Marshal(messages)
	if err != nil {
		return body, false
	}
	envelope["messages"] = messageRaw
	repaired, err := stdjson.Marshal(envelope)
	if err != nil {
		return body, false
	}
	return repaired, true
}
