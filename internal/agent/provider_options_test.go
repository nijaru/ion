package agent

import (
	"context"
	"net/http"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessProviderRequestOptionsAreRequestLocal(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	var mu sync.Mutex
	var headers []map[string]string
	var deadlines []time.Duration
	var transports []http.RoundTripper
	var hookCalls int
	configuredTransport := http.RoundTripper(http.DefaultTransport)
	hookTransport := http.RoundTripper(&http.Transport{})
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Model: llm.Model{
			ID: "test",
			Headers: map[string]string{
				"X-Model":    "model",
				"X-Override": "model",
			},
		},
		Timeout:   time.Second,
		Transport: configuredTransport,
		Auth: func(llm.Model) (string, map[string]string) {
			return "token", map[string]string{"X-Auth": "auth", "X-Override": "auth"}
		},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			deadline, hasDeadline := ctx.Deadline()
			remaining := time.Duration(0)
			if hasDeadline {
				remaining = time.Until(deadline)
			}
			copied := make(map[string]string, len(req.Headers))
			for key, value := range req.Headers {
				copied[key] = value
			}
			mu.Lock()
			transports = append(transports, req.Transport)
			headers = append(headers, copied)
			if !hasDeadline {
				remaining = -1
			}
			deadlines = append(deadlines, remaining)
			mu.Unlock()
			return &mockStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer h.Close()
	shortTimeout := 10 * time.Millisecond
	h.On(HookBeforeProviderRequest, func(payload any) (any, error) {
		mu.Lock()
		defer mu.Unlock()
		hookCalls++
		if hookCalls == 1 {
			return &BeforeProviderRequestPatch{
				Headers:   map[string]string{"X-Hook": "first", "X-Override": "hook"},
				Transport: &hookTransport,
				Timeout:   &shortTimeout,
			}, nil
		}
		return nil, nil
	})

	if _, err := h.Prompt(context.Background(), "first"); err != nil {
		t.Fatal(err)
	}
	if _, err := h.Prompt(context.Background(), "second"); err != nil {
		t.Fatal(err)
	}

	mu.Lock()
	defer mu.Unlock()
	if len(headers) != 2 || len(deadlines) != 2 || len(transports) != 2 {
		t.Fatalf("requests = %d, deadlines = %d, transports = %d, want 2", len(headers), len(deadlines), len(transports))
	}
	if transports[0] != hookTransport || transports[1] != configuredTransport {
		t.Fatalf("transport snapshots = %#v, want hook override then configured transport", transports)
	}
	if deadlines[0] <= 0 || deadlines[0] > 500*time.Millisecond || deadlines[1] < 500*time.Millisecond {
		t.Fatalf("provider timeout windows = %#v, want first short and second configured", deadlines)
	}
	if headers[0]["X-Model"] != "model" || headers[0]["X-Auth"] != "auth" || headers[0]["X-Hook"] != "first" || headers[0]["X-Override"] != "hook" {
		t.Fatalf("first request headers = %#v", headers[0])
	}
	if headers[1]["X-Model"] != "model" || headers[1]["X-Auth"] != "auth" || headers[1]["X-Hook"] != "" || headers[1]["X-Override"] != "auth" {
		t.Fatalf("second request headers inherited request-local patch: %#v", headers[1])
	}
}
