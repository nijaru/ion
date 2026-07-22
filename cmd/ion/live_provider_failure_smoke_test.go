package main

import (
	"bytes"
	"context"
	"errors"
	"fmt"
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
	// Stable custom headers can contain a credential that would defeat this
	// negative probe. The provider must authenticate with the runtime-only
	// invalid token below.
	providerConfig.ExtraHeaders = nil
	provider, err := providers.NewProviderFromConfig(&providerConfig)
	if err != nil {
		t.Fatalf("construct auth probe provider %q: %v", profile.provider, err)
	}

	adapterCtx, cancel := context.WithTimeout(t.Context(), 60*time.Second)
	defer cancel()
	_, err = provider.Generate(adapterCtx, &llm.Request{
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
		t.Fatalf("live auth adapter returned unclassified error type %T", err)
	} else {
		t.Logf("live auth rejection observed: provider=%s status=%d", profile.provider, status)
	}
	if provider.IsTransient(err) {
		t.Fatal("authentication failure was classified as transient")
	}

	path := filepath.Join(t.TempDir(), "live-auth.db")
	store, err := session.NewSQLiteStore(path, "live-auth")
	if err != nil {
		t.Fatalf("open auth probe store: %v", err)
	}
	sess := session.NewSession(store, 128)
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
		StreamFn: provider.Stream,
	})
	sub, err := runner.Subscribe(t.Context(), agent.EventCursor{})
	if err != nil {
		_ = runner.Close()
		_ = store.Close()
		t.Fatalf("subscribe auth events: %v", err)
	}
	t.Cleanup(func() {
		sub.Close()
		if err := runner.Close(); err != nil {
			t.Errorf("cleanup auth runtime: %v", err)
		}
		if err := store.Close(); err != nil {
			t.Errorf("cleanup auth store: %v", err)
		}
	})

	runtimeCtx, runtimeCancel := context.WithTimeout(t.Context(), 90*time.Second)
	defer runtimeCancel()
	_, promptErr := runner.Prompt(runtimeCtx, "auth failure must settle without replay")
	if promptErr == nil {
		t.Fatal("live auth runtime accepted an invalid credential")
	}
	var turnErr *agent.TurnError
	if !errors.As(promptErr, &turnErr) || turnErr.Kind != agent.KindProvider || turnErr.Recovery != agent.RecoveryAbort {
		t.Fatalf("live auth runtime error = %v, want provider TurnError", promptErr)
	}
	if strings.Contains(promptErr.Error(), invalidToken) || strings.Contains(turnErr.Cause.Error(), invalidToken) {
		t.Fatal("invalid runtime credential appeared in runtime error")
	}
	events := drainLiveEvents(sub)
	assertLiveTerminalEvents(t, events, session.StopReasonError)
	record, err := store.LatestTurn(runtimeCtx)
	if err != nil {
		t.Fatalf("read auth turn: %v", err)
	}
	assertLiveTurnRecord(t, record, session.TurnAborted)
	if strings.Contains(record.Error, invalidToken) {
		t.Fatal("invalid runtime credential appeared in durable turn diagnostics")
	}
	if snapshot, err := sess.BuildContext(runtimeCtx); err != nil {
		t.Fatalf("build auth failure context: %v", err)
	} else if len(snapshot.Messages) != 0 {
		t.Fatalf("auth failure replayed messages: %#v", snapshot.Messages)
	}

	if err := runner.Close(); err != nil {
		t.Fatalf("close auth runtime: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close auth store: %v", err)
	}
	reopened, err := session.NewSQLiteStore(path, "live-auth-reopened")
	if err != nil {
		t.Fatalf("reopen auth store: %v", err)
	}
	defer reopened.Close()
	reopenedRecord, err := reopened.GetTurn(runtimeCtx, record.ID)
	if err != nil {
		t.Fatalf("read reopened auth turn: %v", err)
	}
	assertLiveTurnRecord(t, reopenedRecord, session.TurnAborted)
	if interrupted, err := reopened.InterruptedTurns(runtimeCtx); err != nil {
		t.Fatalf("read auth interrupted turns: %v", err)
	} else if len(interrupted) != 0 {
		t.Fatalf("auth failure left interrupted turns: %+v", interrupted)
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
	profile, ok := liveAuthProfile(profiles)
	if !ok {
		t.Fatal("live cancellation probe needs an auth-backed provider profile")
	}
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
		var turnErr *agent.TurnError
		if !errors.As(promptErr, &turnErr) || turnErr.Kind != agent.KindCancellation || turnErr.Recovery != agent.RecoveryAbort {
			t.Fatalf("canceled live provider error = %v, want cancellation TurnError", promptErr)
		}
	case <-ctx.Done():
		t.Fatalf("canceled live provider turn did not settle: %v", ctx.Err())
	}

	events := drainLiveEvents(sub)
	assertLiveTerminalEvents(t, events, session.StopReasonAborted)
	record, err := store.LatestTurn(ctx)
	if err != nil {
		t.Fatalf("read canceled live turn: %v", err)
	}
	assertLiveTurnRecord(t, record, session.TurnAborted)
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
	if err := runner.Close(); err != nil {
		t.Fatalf("close canceled live runtime: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close canceled live store: %v", err)
	}
	reopened, err := session.NewSQLiteStore(path, "live-cancel-reopened")
	if err != nil {
		t.Fatalf("reopen canceled live store: %v", err)
	}
	defer reopened.Close()
	reopenedRecord, err := reopened.GetTurn(ctx, record.ID)
	if err != nil {
		t.Fatalf("read reopened canceled live turn: %v", err)
	}
	assertLiveTurnRecord(t, reopenedRecord, session.TurnAborted)
	if interrupted, err := reopened.InterruptedTurns(ctx); err != nil {
		t.Fatalf("read reopened canceled live interrupted turns: %v", err)
	} else if len(interrupted) != 0 {
		t.Fatalf("reopened canceled live turn left interrupted records: %+v", interrupted)
	}
	t.Logf("live cancellation settled: provider=%s model=%s", profile.provider, profile.model)
}

func liveAuthProfile(profiles []liveProviderProfile) (liveProviderProfile, bool) {
	for _, profile := range profiles {
		definition, ok := llm.Lookup(profile.provider)
		if ok && (definition.AuthKind == llm.AuthAPIKey || definition.AuthKind == llm.AuthToken) {
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
	}
	return 0, false
}

func assertLiveTerminalEvents(t *testing.T, events []session.Event, wantStop session.StopReason) {
	t.Helper()
	turnEnds, agentEnds, settled := 0, 0, 0
	turnEndIndex, agentEndIndex, settledIndex := -1, -1, -1
	for index, event := range events {
		switch event := event.(type) {
		case session.TurnEnd:
			turnEnds++
			turnEndIndex = index
			assistant, ok := event.Message.(*session.AssistantMessage)
			if !ok || assistant.StopReason != wantStop {
				t.Errorf("live terminal TurnEnd message = %#v, want stop reason %q", event.Message, wantStop)
			}
		case session.AgentEnd:
			agentEnds++
			agentEndIndex = index
		case session.Settled:
			settled++
			settledIndex = index
		}
	}
	if turnEnds != 1 || agentEnds != 1 || settled != 1 {
		t.Errorf("live terminal event counts = TurnEnd:%d AgentEnd:%d Settled:%d, want one each", turnEnds, agentEnds, settled)
	}
	if turnEndIndex < 0 || agentEndIndex <= turnEndIndex || settledIndex <= agentEndIndex {
		t.Errorf("live terminal event order = TurnEnd:%d AgentEnd:%d Settled:%d, want TurnEnd < AgentEnd < Settled", turnEndIndex, agentEndIndex, settledIndex)
	}
}

func assertLiveTurnRecord(t *testing.T, record session.TurnRecord, want session.TurnState) {
	t.Helper()
	if record.State != want {
		t.Fatalf("live turn state = %q, want %q", record.State, want)
	}
	if record.EndedAt.IsZero() {
		t.Fatal("live turn has no durable end time")
	}
	if strings.TrimSpace(record.Error) == "" {
		t.Fatal("live terminal turn has no durable outcome reason")
	}
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
	if response.StatusCode != http.StatusOK {
		_ = response.Body.Close()
		return nil, fmt.Errorf("live cancellation response status = %d, want 200", response.StatusCode)
	}
	prefix, err := readLiveSSEPrefix(response.Body)
	if err != nil {
		_ = response.Body.Close()
		return nil, fmt.Errorf("read first live cancellation event: %w", err)
	}
	if bytes.Contains(prefix, []byte("[DONE]")) {
		_ = response.Body.Close()
		return nil, fmt.Errorf("live cancellation response ended before a provider event")
	}
	response.Body = &liveCancellationBody{ReadCloser: response.Body, ctx: req.Context(), prefix: prefix}
	t.once.Do(func() { close(t.started) })
	return response, nil
}

type liveCancellationBody struct {
	io.ReadCloser
	ctx    context.Context
	prefix []byte
}

func (b *liveCancellationBody) Read(p []byte) (int, error) {
	if len(b.prefix) > 0 {
		n := copy(p, b.prefix)
		b.prefix = b.prefix[n:]
		return n, nil
	}
	<-b.ctx.Done()
	return 0, b.ctx.Err()
}

func readLiveSSEPrefix(body io.Reader) ([]byte, error) {
	const maxPrefix = 64 << 10
	var prefix []byte
	buffer := make([]byte, 4096)
	for len(prefix) < maxPrefix {
		n, err := body.Read(buffer)
		if n > 0 {
			prefix = append(prefix, buffer[:n]...)
			if end := bytes.Index(prefix, []byte("\n\n")); end >= 0 {
				return prefix[:end+2], nil
			}
			if end := bytes.Index(prefix, []byte("\r\n\r\n")); end >= 0 {
				return prefix[:end+4], nil
			}
		}
		if err != nil {
			return nil, err
		}
	}
	return nil, fmt.Errorf("first SSE event exceeded %d bytes", maxPrefix)
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
		{name: "server", err: llm.NewHTTPError("test", http.StatusBadGateway, nil), want: 0, ok: false},
		{name: "untyped-401-text", err: errors.New("status code: 401"), want: 0, ok: false},
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

func TestReadLiveSSEPrefix(t *testing.T) {
	for _, input := range []string{
		"data: first\n\ndata: second\n\n",
		"data: first\r\n\r\ndata: second\r\n\r\n",
	} {
		prefix, err := readLiveSSEPrefix(strings.NewReader(input))
		if err != nil {
			t.Fatalf("readLiveSSEPrefix(%q): %v", input, err)
		}
		if !strings.Contains(string(prefix), "data: first") || strings.Contains(string(prefix), "data: second") {
			t.Fatalf("prefix = %q, want only first SSE event", prefix)
		}
	}
}
