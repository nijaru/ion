package main

import (
	"context"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers"
	"github.com/nijaru/ion/session"
)

// TestLiveProviderAuthenticationFailure is an opt-in negative live probe. It
// sends one request with a runtime-only invalid token and verifies that the
// real provider rejects it as authentication failure. It does not use the
// production retry wrapper because authentication errors are not retryable.
func TestLiveProviderAuthenticationFailure(t *testing.T) {
	if os.Getenv("ION_LIVE_SMOKE") != "1" || os.Getenv("ION_LIVE_AUTH_FAILURE") != "1" {
		t.Skip("set ION_LIVE_SMOKE=1 and ION_LIVE_AUTH_FAILURE=1 to run the opt-in live auth probe")
	}
	profiles, err := loadLiveProviderProfiles()
	if err != nil {
		t.Fatal(err)
	}
	profile, ok := liveAuthProfile(profiles)
	if !ok {
		t.Fatal("live auth probe needs an auth-backed provider profile")
	}

	const invalidToken = "ion-invalid-live-auth-probe"
	providerConfig := profile.providerConfig
	providerConfig.APIKeyOverride = invalidToken
	providerConfig.APIKeyOverrideProvider = profile.provider
	provider, err := providers.NewProviderFromConfig(&providerConfig)
	if err != nil {
		t.Fatalf("construct auth probe provider %q: %v", profile.provider, err)
	}

	ctx, cancel := context.WithTimeout(t.Context(), 60*time.Second)
	defer cancel()
	_, err = provider.Generate(ctx, &llm.Request{
		Model:    profile.model,
		Messages: []llm.Message{{Role: llm.RoleUser, Content: "auth probe"}},
	})
	if err == nil {
		t.Fatal("invalid live credential was accepted")
	}
	if strings.Contains(err.Error(), invalidToken) {
		t.Fatal("invalid runtime credential appeared in provider error")
	}
	if status, ok := liveAuthFailureStatus(err); !ok {
		t.Fatalf("live auth probe returned unclassified error type %T", err)
	} else {
		t.Logf("live auth rejection observed: provider=%s status=%d", profile.provider, status)
	}
}

// TestLiveProviderCancellation is an opt-in live cancellation probe. The
// transport waits at the response-body boundary after the real provider has
// received response headers. Abort then cancels the provider request context;
// the runtime must settle and leave the explicitly canceled turn out of
// replayable session history.
func TestLiveProviderCancellation(t *testing.T) {
	if os.Getenv("ION_LIVE_SMOKE") != "1" || os.Getenv("ION_LIVE_CANCELLATION") != "1" {
		t.Skip("set ION_LIVE_SMOKE=1 and ION_LIVE_CANCELLATION=1 to run the opt-in live cancellation probe")
	}
	profiles, err := loadLiveProviderProfiles()
	if err != nil {
		t.Fatal(err)
	}
	profile := profiles[0]
	providerConfig := profile.providerConfig
	provider, err := providers.NewProviderFromConfig(&providerConfig)
	if err != nil {
		t.Fatalf("construct cancellation provider %q: %v", profile.provider, err)
	}
	provider = providerWithRetryPolicy(provider, &providerConfig)
	if !provider.Capabilities(profile.model).Streaming {
		t.Fatalf("provider %q model %q does not advertise streaming", profile.provider, profile.model)
	}

	path := filepath.Join(t.TempDir(), "live-cancel.db")
	store, err := session.NewSQLiteStore(path, "live-cancel")
	if err != nil {
		t.Fatalf("open cancellation store: %v", err)
	}
	sess := session.NewSession(store, 128)
	transport := &liveCancellationTransport{base: http.DefaultTransport, started: make(chan struct{})}
	runner := agent.NewController(agent.ControllerConfig{
		Session:        sess,
		Store:          store,
		Durable:        store,
		RequireDurable: true,
		Model: llm.Model{
			ID:            profile.model,
			Provider:      profile.provider,
			BaseURL:       profile.endpoint,
			ContextWindow: profile.contextWindow,
		},
		Transport: transport,
		StreamFn:  provider.Stream,
	})
	sub, err := runner.Subscribe(t.Context(), agent.EventCursor{})
	if err != nil {
		_ = runner.Close()
		_ = store.Close()
		t.Fatalf("subscribe cancellation events: %v", err)
	}
	t.Cleanup(func() {
		sub.Close()
		if err := runner.Close(); err != nil {
			t.Errorf("cleanup cancellation runtime: %v", err)
		}
		if err := store.Close(); err != nil {
			t.Errorf("cleanup cancellation store: %v", err)
		}
	})

	ctx, cancel := context.WithTimeout(t.Context(), 90*time.Second)
	defer cancel()
	promptDone := make(chan error, 1)
	go func() {
		_, promptErr := runner.Prompt(ctx, "Begin a long response, then continue until canceled.")
		promptDone <- promptErr
	}()
	select {
	case <-transport.started:
	case <-ctx.Done():
		t.Fatalf("live provider request did not reach response headers: %v", ctx.Err())
	}
	if _, _, err := runner.Abort(); err != nil {
		t.Fatalf("abort live provider turn: %v", err)
	}
	select {
	case promptErr := <-promptDone:
		if promptErr == nil {
			t.Fatal("canceled live provider turn returned nil error")
		}
	case <-ctx.Done():
		t.Fatalf("canceled live provider turn did not settle: %v", ctx.Err())
	}

	events := drainLiveEvents(sub)
	if !hasAbortedLiveTurn(events) {
		t.Fatal("canceled live turn emitted no aborted TurnEnd")
	}
	if !hasLiveSettledEvent(events) {
		t.Fatal("canceled live turn emitted no Settled event")
	}
	snapshot, err := sess.BuildContext(ctx)
	if err != nil {
		t.Fatalf("build canceled live context: %v", err)
	}
	if len(snapshot.Messages) != 0 {
		t.Fatalf("canceled live turn replayed messages: %#v", snapshot.Messages)
	}
	if interrupted, err := store.InterruptedTurns(ctx); err != nil {
		t.Fatalf("read canceled live interrupted turns: %v", err)
	} else if len(interrupted) != 0 {
		t.Fatalf("explicitly canceled live turn left interrupted records: %+v", interrupted)
	}
	t.Logf("live cancellation settled: provider=%s model=%s", profile.provider, profile.model)
}

func liveAuthProfile(profiles []liveProviderProfile) (liveProviderProfile, bool) {
	for _, profile := range profiles {
		definition, ok := llm.Lookup(profile.provider)
		if ok && definition.AuthKind != llm.AuthLocal {
			return profile, true
		}
	}
	return liveProviderProfile{}, false
}

func liveAuthFailureStatus(err error) (int, bool) {
	if status, ok := llm.HTTPStatusCode(err); ok {
		if status == http.StatusUnauthorized || status == http.StatusForbidden {
			return status, true
		}
		return 0, false
	}
	message := strings.ToLower(err.Error())
	if strings.Contains(message, "401") || strings.Contains(message, "unauthor") {
		return http.StatusUnauthorized, true
	}
	if strings.Contains(message, "403") || strings.Contains(message, "forbidden") {
		return http.StatusForbidden, true
	}
	return 0, false
}

func hasAbortedLiveTurn(events []session.Event) bool {
	for _, event := range events {
		turnEnd, ok := event.(session.TurnEnd)
		if !ok {
			continue
		}
		assistant, ok := turnEnd.Message.(*session.AssistantMessage)
		if ok && assistant.StopReason == session.StopReasonAborted {
			return true
		}
	}
	return false
}

func hasLiveSettledEvent(events []session.Event) bool {
	for _, event := range events {
		if _, ok := event.(session.Settled); ok {
			return true
		}
	}
	return false
}

type liveCancellationTransport struct {
	base    http.RoundTripper
	started chan struct{}
	once    sync.Once
}

func (t *liveCancellationTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	base := t.base
	if base == nil {
		base = http.DefaultTransport
	}
	response, err := base.RoundTrip(req)
	if err != nil {
		return nil, err
	}
	response.Body = &liveCancellationBody{ReadCloser: response.Body, ctx: req.Context()}
	t.once.Do(func() { close(t.started) })
	return response, nil
}

type liveCancellationBody struct {
	io.ReadCloser
	ctx context.Context
}

func (b *liveCancellationBody) Read(p []byte) (int, error) {
	<-b.ctx.Done()
	return 0, b.ctx.Err()
}

func TestLiveAuthFailureStatus(t *testing.T) {
	cases := []struct {
		name string
		err  error
		want int
		ok   bool
	}{
		{name: "http-401", err: llm.NewHTTPError("test", http.StatusUnauthorized, nil), want: http.StatusUnauthorized, ok: true},
		{name: "http-403", err: llm.NewHTTPError("test", http.StatusForbidden, nil), want: http.StatusForbidden, ok: true},
		{name: "sdk-401-text", err: errors.New("status code: 401"), want: http.StatusUnauthorized, ok: true},
		{name: "server", err: llm.NewHTTPError("test", http.StatusBadGateway, nil), want: 0, ok: false},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			got, ok := liveAuthFailureStatus(test.err)
			if got != test.want || ok != test.ok {
				t.Fatalf("liveAuthFailureStatus() = (%d, %v), want (%d, %v)", got, ok, test.want, test.ok)
			}
		})
	}
}

func TestLiveCancellationBodyUnblocksOnContextCancel(t *testing.T) {
	ctx, cancel := context.WithCancel(t.Context())
	body := &liveCancellationBody{ReadCloser: io.NopCloser(strings.NewReader("unused")), ctx: ctx}
	result := make(chan error, 1)
	go func() {
		_, err := body.Read(make([]byte, 1))
		result <- err
	}()
	cancel()
	select {
	case err := <-result:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("Read() error = %v, want context canceled", err)
		}
	case <-time.After(time.Second):
		t.Fatal("cancellation body remained blocked")
	}
}
