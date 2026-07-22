package openrouter

import (
	"io"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
)

type openRouterTransport struct {
	responses   []string
	statusCodes []int
	calls       int
}

func (t *openRouterTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	index := t.calls
	body := t.responses[index]
	status := http.StatusOK
	if index < len(t.statusCodes) && t.statusCodes[index] != 0 {
		status = t.statusCodes[index]
	}
	t.calls++
	return &http.Response{StatusCode: status, Body: io.NopCloser(strings.NewReader(body)), Header: make(http.Header), Request: req}, nil
}

func TestRequestTransportUsedForGenerateAndStream(t *testing.T) {
	transport := &openRouterTransport{responses: []string{
		`{"choices":[{"message":{"content":"generated"}}],"usage":{}}`,
		"data: {\"choices\":[{\"delta\":{\"content\":\"streamed\"}}]}\n\ndata: [DONE]\n\n",
	}}
	p := NewProvider(llm.ProviderConfig{APIKey: "test", APIEndpoint: "https://example.test/v1"})
	req := &llm.Request{Model: "test", Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}}, Transport: transport}

	if _, err := p.Generate(t.Context(), req); err != nil {
		t.Fatal(err)
	}
	stream, err := p.Stream(t.Context(), req)
	if err != nil {
		t.Fatal(err)
	}
	if chunk, ok := stream.Next(); !ok || chunk.Content != "streamed" {
		t.Fatalf("stream chunk = %#v, ok=%v", chunk, ok)
	}
	if transport.calls != 2 {
		t.Fatalf("transport calls = %d, want 2", transport.calls)
	}
	_ = stream.Close()
}

func TestRetryProviderRetriesOpenRouterRateLimitBeforeStreaming(t *testing.T) {
	transport := &openRouterTransport{
		responses: []string{
			`{"error":{"message":"rate limited"}}`,
			"data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"}}]}\n\ndata: [DONE]\n\n",
		},
		statusCodes: []int{http.StatusTooManyRequests, http.StatusOK},
	}
	provider := NewProvider(llm.ProviderConfig{
		APIKey:      "test",
		APIEndpoint: "https://example.test/v1",
		Models:      []llm.Model{{ID: "test", Capabilities: &llm.Capabilities{Streaming: true}}},
	})
	retry := llm.NewRetryProvider(provider)
	retry.Config = llm.RetryConfig{
		MaxAttempts: 2,
		MinInterval: time.Nanosecond,
		MaxInterval: time.Nanosecond,
		Multiplier:  1,
	}
	var retries []llm.RetryEvent
	ctx := llm.WithRetryObserver(t.Context(), func(event llm.RetryEvent) {
		retries = append(retries, event)
	})
	stream, err := retry.Stream(ctx, &llm.Request{
		Model:     "test",
		Messages:  []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
		Transport: transport,
	})
	if err != nil {
		t.Fatalf("retry Stream: %v", err)
	}
	defer stream.Close()
	if chunk, ok := stream.Next(); !ok || chunk.Content != "recovered" {
		t.Fatalf("stream chunk = %#v, ok=%v; want recovered", chunk, ok)
	}
	if transport.calls != 2 {
		t.Fatalf("transport calls = %d, want 2", transport.calls)
	}
	if len(retries) != 1 || retries[0].Attempt != 1 {
		t.Fatalf("retry events = %#v, want one attempt-1 event", retries)
	}
}

func TestOpenRouterStreamAdapterReportsMalformedSSE(t *testing.T) {
	transport := &openRouterTransport{responses: []string{"data: {not-json}\n"}}
	provider := NewProvider(llm.ProviderConfig{
		APIKey:      "test",
		APIEndpoint: "https://example.test/v1",
		Models:      []llm.Model{{ID: "test", Capabilities: &llm.Capabilities{Streaming: true}}},
	})
	stream, err := provider.Stream(t.Context(), &llm.Request{
		Model:     "test",
		Messages:  []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
		Transport: transport,
	})
	if err != nil {
		t.Fatalf("Stream: %v", err)
	}
	defer stream.Close()
	if chunk, ok := stream.Next(); ok || chunk != nil {
		t.Fatalf("Next() = (%#v, %v), want terminal malformed error", chunk, ok)
	}
	if stream.Err() == nil {
		t.Fatal("stream.Err() = nil, want malformed SSE error")
	}
}

func TestOpenRouterContextOverflowResponseIsClassified(t *testing.T) {
	transport := &openRouterTransport{
		responses:   []string{`{"error":{"message":"maximum context window exceeded: too many tokens"}}`},
		statusCodes: []int{http.StatusBadRequest},
	}
	provider := NewProvider(llm.ProviderConfig{
		APIKey:      "test",
		APIEndpoint: "https://example.test/v1",
		Models:      []llm.Model{{ID: "test", Capabilities: &llm.Capabilities{Streaming: true}}},
	})
	_, err := provider.Stream(t.Context(), &llm.Request{
		Model:     "test",
		Messages:  []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
		Transport: transport,
	})
	if err == nil {
		t.Fatal("Stream error = nil, want context overflow")
	}
	if !provider.IsContextOverflow(err) {
		t.Fatalf("provider did not classify context overflow: %v", err)
	}
}
