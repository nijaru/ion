package openai

import (
	"errors"
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

type captureRequestTransport struct {
	body []byte
}

func (t *captureRequestTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	var err error
	t.body, err = io.ReadAll(req.Body)
	if err != nil {
		return nil, err
	}
	return &http.Response{
		StatusCode: http.StatusOK,
		Body: io.NopCloser(strings.NewReader(
			"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
		)),
		Header:  make(http.Header),
		Request: req,
	}, nil
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

func TestStreamingOptionsAreLimitedToStreamingRequests(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{APIKey: "test", APIEndpoint: "https://example.test/v1"})
	req := &llm.Request{Model: "test", Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}}}
	if got := p.ConvertRequest(req).StreamOptions; got != nil {
		t.Fatal("non-streaming request includes stream options")
	}
	if got := p.ConvertStreamingRequest(req).StreamOptions; got == nil || !got.IncludeUsage {
		t.Fatalf("streaming options = %#v, want include_usage", got)
	}
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

type closeErrorBody struct {
	io.Reader
	err error
}

func (b closeErrorBody) Close() error { return b.err }

type closeErrorTransport struct {
	body io.ReadCloser
}

func (t closeErrorTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	return &http.Response{
		StatusCode: http.StatusOK,
		Body:       t.body,
		Header:     make(http.Header),
		Request:    req,
	}, nil
}

func TestStreamUsesRequestCapabilitySnapshot(t *testing.T) {
	transport := &captureRequestTransport{}
	p := NewProvider(llm.ProviderConfig{APIKey: "test", APIEndpoint: "https://example.test/v1"})
	caps := llm.DefaultCapabilities()
	caps.InputModalities = []string{"text"}
	stream, err := p.Stream(t.Context(), &llm.Request{
		Model: "test",
		Messages: []llm.Message{{
			Role: llm.RoleUser,
			Parts: []llm.ContentPart{
				llm.TextPart("describe this"),
				llm.ImagePart("image/png", "aW1hZ2U="),
			},
		}},
		CapabilitySnapshot: &caps,
		Transport:          transport,
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := stream.Next(); !ok {
		t.Fatal("stream ended before response")
	}
	if err := stream.Close(); err != nil {
		t.Fatal(err)
	}
	body := string(transport.body)
	if strings.Contains(body, "image_url") {
		t.Fatalf("request retained image content despite text-only snapshot: %s", body)
	}
	if !strings.Contains(body, "image omitted: model does not accept image input") {
		t.Fatalf("request omitted explicit image downgrade: %s", body)
	}
}

func TestStreamCloseReportsBodyFailure(t *testing.T) {
	closeErr := errors.New("stream body close failed")
	p := NewProvider(llm.ProviderConfig{APIKey: "test", APIEndpoint: "https://example.test/v1"})
	stream, err := p.Stream(t.Context(), &llm.Request{
		Model: "test",
		Messages: []llm.Message{{
			Role:    llm.RoleUser,
			Content: "hello",
		}},
		Transport: closeErrorTransport{body: closeErrorBody{
			Reader: strings.NewReader("data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\ndata: [DONE]\n\n"),
			err:    closeErr,
		}},
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := stream.Next(); !ok {
		t.Fatal("stream ended before content")
	}
	for {
		if _, ok := stream.Next(); !ok {
			break
		}
	}
	if err := stream.Close(); !errors.Is(err, closeErr) {
		t.Fatalf("stream close error = %v, want %v", err, closeErr)
	}
}
