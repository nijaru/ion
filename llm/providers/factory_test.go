package providers

import (
	"context"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
)

type authCaptureTransport struct {
	authorization string
}

func (t *authCaptureTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	t.authorization = req.Header.Get("Authorization")
	return &http.Response{
		StatusCode: http.StatusOK,
		Header:     make(http.Header),
		Body:       io.NopCloser(strings.NewReader("")),
		Request:    req,
	}, nil
}

func TestNewProviderFromConfigUsesRuntimeAPIKeyOverride(t *testing.T) {
	provider, err := NewProviderFromConfig(&config.Config{
		Provider:               "openai",
		Model:                  "test-model",
		Endpoint:               "https://example.test/v1",
		APIKeyOverride:         "runtime-key",
		APIKeyOverrideProvider: "openai",
	})
	if err != nil {
		t.Fatal(err)
	}
	transport := &authCaptureTransport{}
	stream, err := provider.Stream(context.Background(), &llm.Request{
		Model:     "test-model",
		Transport: transport,
	})
	if err != nil {
		t.Fatal(err)
	}
	_ = stream.Close()
	if transport.authorization != "Bearer runtime-key" {
		t.Fatalf("authorization = %q, want Bearer runtime-key", transport.authorization)
	}
}
