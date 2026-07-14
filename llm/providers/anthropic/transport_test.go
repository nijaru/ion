package anthropic

import (
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
)

type anthropicTransport struct {
	body  string
	calls int
}

func (t *anthropicTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	t.calls++
	return &http.Response{StatusCode: http.StatusOK, Body: io.NopCloser(strings.NewReader(t.body)), Header: http.Header{"Content-Type": []string{"application/json"}}, Request: req}, nil
}

func TestRequestTransportUsedForGenerate(t *testing.T) {
	transport := &anthropicTransport{body: `{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"generated"}],"model":"test","stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}`}
	p := NewProvider(llm.ProviderConfig{APIKey: "test", APIEndpoint: "https://example.test"})
	req := &llm.Request{Model: "test", Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}}, Transport: transport}

	resp, err := p.Generate(t.Context(), req)
	if err != nil {
		t.Fatal(err)
	}
	if resp.Blocks == nil || transport.calls != 1 {
		t.Fatalf("response = %#v, transport calls = %d", resp, transport.calls)
	}
}

func TestRequestTransportUsedForStream(t *testing.T) {
	transport := &anthropicTransport{body: "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"streamed\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"}
	p := NewProvider(llm.ProviderConfig{APIKey: "test", APIEndpoint: "https://example.test"})
	req := &llm.Request{Model: "test", Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}}, Transport: transport}

	stream, err := p.Stream(t.Context(), req)
	if err != nil {
		t.Fatal(err)
	}
	defer stream.Close()
	var content string
	for {
		chunk, ok := stream.Next()
		if !ok {
			break
		}
		content += chunk.Content
	}
	if err := stream.Err(); err != nil {
		t.Fatal(err)
	}
	if content != "streamed" || transport.calls != 1 {
		t.Fatalf("content = %q, transport calls = %d", content, transport.calls)
	}
}
