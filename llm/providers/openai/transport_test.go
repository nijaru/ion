package openai

import (
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
)

type responseTransport struct {
	responses []string
	calls     int
}

func (t *responseTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	body := t.responses[t.calls]
	t.calls++
	return &http.Response{
		StatusCode: http.StatusOK,
		Body:       io.NopCloser(strings.NewReader(body)),
		Header:     make(http.Header),
		Request:    req,
	}, nil
}

func TestRequestTransportUsedForGenerateAndStream(t *testing.T) {
	transport := &responseTransport{responses: []string{
		`{"choices":[{"message":{"content":"generated"}}],"usage":{}}`,
		"data: {\"choices\":[{\"delta\":{\"content\":\"streamed\"}}]}\n\ndata: [DONE]\n\n",
	}}
	p := NewProvider(llm.ProviderConfig{APIKey: "test", APIEndpoint: "https://example.test/v1"})
	req := &llm.Request{
		Model:     "test",
		Messages:  []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
		Transport: transport,
	}

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
