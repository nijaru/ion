package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers"
	"github.com/nijaru/ion/session"
)

// TestLiveSmokeTurnAndToolCall is the opt-in live-provider conformance gate.
//
// The default test suite never makes network requests. When ION_LIVE_SMOKE=1,
// profile A comes from the stable local config unless explicitly overridden,
// and profile B must be supplied with ION_LIVE_PROVIDER_B and
// ION_LIVE_MODEL_B. Provider adapters resolve credentials from their normal
// environment variables; this test never accepts, prints, or persists a key.
func TestLiveSmokeTurnAndToolCall(t *testing.T) {
	if os.Getenv("ION_LIVE_SMOKE") != "1" {
		t.Skip("set ION_LIVE_SMOKE=1 to run the opt-in live-provider gate")
	}

	profiles, err := loadLiveProviderProfiles()
	if err != nil {
		t.Fatal(err)
	}
	for _, profile := range profiles {
		profile := profile
		t.Run(profile.name, func(t *testing.T) {
			runLiveProviderTurn(t, profile)
		})
	}
}

// TestLiveBasicTurn is the low-cost provider smoke used while onboarding a
// model or endpoint. It deliberately permits two profiles from the same
// adapter so a paid/free endpoint comparison can exercise text streaming,
// settlement, persistence, and replay without claiming full provider
// conformance.
func TestLiveBasicTurn(t *testing.T) {
	if os.Getenv("ION_LIVE_BASIC") != "1" {
		t.Skip("set ION_LIVE_BASIC=1 to run the opt-in basic live-provider smoke")
	}

	profiles, err := loadLiveProviderProfilesAllowSameAdapter()
	if err != nil {
		t.Fatal(err)
	}
	for _, profile := range profiles {
		profile := profile
		t.Run(profile.name, func(t *testing.T) {
			runLiveProviderBasicTurn(t, profile)
		})
	}
}

type liveProviderProfile struct {
	name           string
	provider       string
	model          string
	endpoint       string
	contextWindow  int
	thinking       session.ThinkingLevel
	providerConfig config.Config
}

func loadLiveProviderProfiles() ([]liveProviderProfile, error) {
	return loadLiveProviderProfilesWithDistinctAdapter(true)
}

func loadLiveProviderProfilesAllowSameAdapter() ([]liveProviderProfile, error) {
	return loadLiveProviderProfilesWithDistinctAdapter(false)
}

func loadLiveProviderProfilesWithDistinctAdapter(requireDistinctAdapter bool) ([]liveProviderProfile, error) {
	stable, err := config.LoadStable()
	if err != nil {
		return nil, fmt.Errorf("load stable config for live provider A: %w", err)
	}

	providerA := strings.TrimSpace(os.Getenv("ION_LIVE_PROVIDER_A"))
	if providerA == "" {
		providerA = strings.TrimSpace(stable.Provider)
	}
	modelA := strings.TrimSpace(os.Getenv("ION_LIVE_MODEL_A"))
	if modelA == "" {
		modelA = strings.TrimSpace(stable.Model)
	}
	providerB := strings.TrimSpace(os.Getenv("ION_LIVE_PROVIDER_B"))
	modelB := strings.TrimSpace(os.Getenv("ION_LIVE_MODEL_B"))
	if providerA == "" || modelA == "" {
		return nil, fmt.Errorf(
			"live provider A needs ION_LIVE_PROVIDER_A and ION_LIVE_MODEL_A, or a configured provider/model",
		)
	}
	if providerB == "" || modelB == "" {
		return nil, fmt.Errorf(
			"live provider B needs ION_LIVE_PROVIDER_B and ION_LIVE_MODEL_B; two explicit provider profiles are required",
		)
	}
	if requireDistinctAdapter && llm.ResolveID(providerA) == llm.ResolveID(providerB) {
		return nil, fmt.Errorf(
			"live provider A and B must use materially different provider adapters, got %q and %q",
			providerA,
			providerB,
		)
	}
	providerA = llm.ResolveID(providerA)
	providerB = llm.ResolveID(providerB)

	contextWindow := stable.ContextLimit
	if contextWindow <= 0 {
		contextWindow = 128000
	}
	endpointA := strings.TrimSpace(os.Getenv("ION_LIVE_ENDPOINT_A"))
	if endpointA == "" {
		endpointA = stable.Endpoint
	}
	thinkingA := liveThinkingLevel("ION_LIVE_THINKING_A", stable.ReasoningEffort)
	configA := *stable
	configA.Provider = providerA
	configA.Model = modelA
	configA.Endpoint = endpointA
	configA.ContextLimit = contextWindow
	// Stable config is file-backed and must not carry runtime-only credentials,
	// but clear these fields defensively before handing it to a live probe.
	configA.APIKeyOverride = ""
	configA.APIKeyOverrideProvider = ""
	configB := config.Config{
		Provider:     providerB,
		Model:        modelB,
		Endpoint:     strings.TrimSpace(os.Getenv("ION_LIVE_ENDPOINT_B")),
		ContextLimit: contextWindow,
	}
	return []liveProviderProfile{
		{
			name:           "a-" + llm.ResolveID(providerA),
			provider:       providerA,
			model:          modelA,
			endpoint:       endpointA,
			contextWindow:  contextWindow,
			thinking:       thinkingA,
			providerConfig: configA,
		},
		{
			name:           "b-" + llm.ResolveID(providerB),
			provider:       providerB,
			model:          modelB,
			endpoint:       strings.TrimSpace(os.Getenv("ION_LIVE_ENDPOINT_B")),
			contextWindow:  contextWindow,
			thinking:       liveThinkingLevel("ION_LIVE_THINKING_B", "auto"),
			providerConfig: configB,
		},
	}, nil
}

func liveThinkingLevel(envName, fallback string) session.ThinkingLevel {
	value := strings.TrimSpace(strings.ToLower(os.Getenv(envName)))
	if value == "" {
		value = strings.TrimSpace(strings.ToLower(fallback))
	}
	if value == "" {
		value = string(session.ThinkingAuto)
	}
	return session.ThinkingLevel(value)
}

func runLiveProviderTurn(t *testing.T, profile liveProviderProfile) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 180*time.Second)
	defer cancel()

	providerConfig := profile.providerConfig
	endpointResolver := llm.NewEndpointResolver(llm.EndpointResolverOptions{})
	provider, err := providers.NewProviderFromConfig(ctx, &providerConfig, endpointResolver)
	if err != nil {
		t.Fatalf("construct provider %q: %v", profile.provider, err)
	}
	provider = providerWithRetryPolicy(provider, &providerConfig)
	caps := provider.Capabilities(profile.model)
	if !caps.Streaming || !caps.Tools {
		t.Fatalf(
			"provider %q model %q capabilities = %#v, need streaming and tools for this conformance profile",
			profile.provider,
			profile.model,
			caps,
		)
	}
	if profile.thinking != session.ThinkingAuto && !caps.SupportsReasoningControl(string(profile.thinking)) {
		t.Fatalf(
			"provider %q model %q does not advertise configured thinking level %q: %#v",
			profile.provider,
			profile.model,
			profile.thinking,
			caps,
		)
	}

	path := filepath.Join(t.TempDir(), "live.db")
	store, err := session.NewSQLiteStore(path, "live-"+profile.name)
	if err != nil {
		t.Fatalf("open durable store: %v", err)
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
		Thinking: profile.thinking,
		Tools:    []agent.Tool{liveEchoTool()},
		StreamFn: provider.Stream,
	})
	t.Cleanup(func() {
		if err := runner.Close(); err != nil {
			t.Errorf("cleanup live runtime: %v", err)
		}
		if err := store.Close(); err != nil {
			t.Errorf("cleanup live store: %v", err)
		}
	})

	sub, err := runner.Subscribe(ctx, agent.EventCursor{})
	if err != nil {
		_ = runner.Close()
		_ = store.Close()
		t.Fatalf("subscribe before live turn: %v", err)
	}
	t.Cleanup(sub.Close)

	prompt := "Use the ion_live_echo tool exactly once with the short text live-check, then answer with a concise confirmation."
	response, promptErr := runner.Prompt(ctx, prompt)
	events := drainLiveEvents(sub)
	if err := sub.Err(); err != nil {
		t.Fatalf("live event subscription: %v", err)
	}
	sub.Close()
	if promptErr != nil {
		_ = runner.Close()
		_ = store.Close()
		t.Fatalf("live turn through %s/%s: %v", profile.provider, profile.model, promptErr)
	}

	assertLiveEvents(t, events, profile)
	if response == nil {
		t.Fatal("live Prompt returned a nil response")
	}

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("read live durable entries: %v", err)
	}
	final := assertLiveDurableTurn(t, entries, profile)
	if got := session.MessageText(response); strings.TrimSpace(got) == "" {
		t.Fatalf("live Prompt response has no text: %#v", response)
	}
	if got := session.MessageText(&final); strings.TrimSpace(got) == "" {
		t.Fatalf("durable final assistant response has no text: %#v", final)
	}

	if err := runner.Close(); err != nil {
		t.Fatalf("close live runtime: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close live store: %v", err)
	}

	reopened, err := session.NewSQLiteStore(path, "reopened-identity-is-ignored")
	if err != nil {
		t.Fatalf("reopen live store: %v", err)
	}
	defer reopened.Close()
	resumed := session.NewSession(reopened, 128)
	snapshot, err := resumed.BuildContext(ctx)
	if err != nil {
		t.Fatalf("build context after live restart: %v", err)
	}
	if !containsLiveText(snapshot.Messages, "live-check") {
		t.Fatalf("replayed context lost the live tool result: %v", liveMessageSummary(snapshot.Messages))
	}
	if !containsLiveText(snapshot.Messages, session.MessageText(&final)) {
		t.Fatalf("replayed context lost the final live response: %v", liveMessageSummary(snapshot.Messages))
	}

	// Constructing a fresh controller against the reopened durable session
	// proves restart composition without making a second paid provider call.
	restartedProviderConfig := profile.providerConfig
	restartedProvider, err := providers.NewProviderFromConfig(ctx, &restartedProviderConfig, endpointResolver)
	if err != nil {
		t.Fatalf("construct provider for restart: %v", err)
	}
	restartedProvider = providerWithRetryPolicy(restartedProvider, &restartedProviderConfig)
	restarted := agent.NewController(agent.ControllerConfig{
		Session:        resumed,
		Store:          reopened,
		Durable:        reopened,
		RequireDurable: true,
		Model: llm.Model{
			ID:            profile.model,
			Provider:      profile.provider,
			ContextWindow: profile.contextWindow,
		},
		Tools:    []agent.Tool{liveEchoTool()},
		StreamFn: restartedProvider.Stream,
	})
	t.Cleanup(func() {
		if err := restarted.Close(); err != nil {
			t.Errorf("cleanup restarted live runtime: %v", err)
		}
	})
	if err := restarted.Close(); err != nil {
		t.Fatalf("close restarted live runtime: %v", err)
	}
	t.Logf(
		"live profile passed turn/tool/metadata/durable-replay: provider=%s model=%s",
		profile.provider,
		profile.model,
	)
}

func runLiveProviderBasicTurn(t *testing.T, profile liveProviderProfile) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 120*time.Second)
	defer cancel()

	providerConfig := profile.providerConfig
	endpointResolver := llm.NewEndpointResolver(llm.EndpointResolverOptions{})
	provider, err := providers.NewProviderFromConfig(ctx, &providerConfig, endpointResolver)
	if err != nil {
		t.Fatalf("construct provider %q: %v", profile.provider, err)
	}
	provider = providerWithRetryPolicy(provider, &providerConfig)
	if !provider.Capabilities(profile.model).Streaming {
		t.Fatalf("provider %q model %q does not advertise streaming", profile.provider, profile.model)
	}

	path := filepath.Join(t.TempDir(), "live-basic.db")
	store, err := session.NewSQLiteStore(path, "live-basic-"+profile.name)
	if err != nil {
		t.Fatalf("open durable store: %v", err)
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
		Thinking: profile.thinking,
		StreamFn: provider.Stream,
	})
	t.Cleanup(func() {
		if err := runner.Close(); err != nil {
			t.Errorf("cleanup basic live runtime: %v", err)
		}
		if err := store.Close(); err != nil {
			t.Errorf("cleanup basic live store: %v", err)
		}
	})

	sub, err := runner.Subscribe(ctx, agent.EventCursor{})
	if err != nil {
		t.Fatalf("subscribe before basic live turn: %v", err)
	}
	t.Cleanup(sub.Close)
	response, promptErr := runner.Prompt(ctx, `Reply with the exact marker "ion-basic-ok".`)
	events := drainLiveEvents(sub)
	if promptErr != nil {
		t.Fatalf("basic live turn through %s/%s: %v", profile.provider, profile.model, promptErr)
	}
	if response == nil || !strings.Contains(session.MessageText(response), "ion-basic-ok") {
		t.Fatalf("basic live response = %#v, want marker ion-basic-ok", response)
	}
	assertBasicLiveEvents(t, events)

	entries, err := sess.Entries(ctx)
	if err != nil {
		t.Fatalf("read basic live durable entries: %v", err)
	}
	var final *session.AssistantMessage
	for _, entry := range entries {
		messageEntry, ok := entry.(*session.MessageEntry)
		if !ok {
			continue
		}
		if assistant, ok := messageEntry.Message.(*session.AssistantMessage); ok {
			final = assistant
		}
	}
	if final == nil || !strings.Contains(session.MessageText(final), "ion-basic-ok") {
		t.Fatalf("basic durable response = %#v, want marker ion-basic-ok", final)
	}

	if err := runner.Close(); err != nil {
		t.Fatalf("close basic live runtime: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close basic live store: %v", err)
	}
	reopened, err := session.NewSQLiteStore(path, "reopened-basic-identity-is-ignored")
	if err != nil {
		t.Fatalf("reopen basic live store: %v", err)
	}
	defer reopened.Close()
	snapshot, err := session.NewSession(reopened, 128).BuildContext(ctx)
	if err != nil {
		t.Fatalf("build basic context after restart: %v", err)
	}
	if !containsLiveText(snapshot.Messages, "ion-basic-ok") {
		t.Fatalf("basic replay lost response marker: %s", liveMessageSummary(snapshot.Messages))
	}
	t.Logf(
		"basic live profile passed text/settlement/persistence/replay: provider=%s model=%s",
		profile.provider,
		profile.model,
	)
}

func assertBasicLiveEvents(t *testing.T, events []session.Event) {
	t.Helper()
	text, settled := false, false
	for _, event := range events {
		switch event := event.(type) {
		case session.MessageUpdate:
			if _, ok := event.Delta.(session.TextDelta); ok {
				text = true
			}
		case session.Settled:
			settled = true
		}
	}
	if !text {
		t.Error("basic live event stream contained no text delta")
	}
	if !settled {
		t.Error("basic live event stream contained no Settled event")
	}
}

func liveEchoTool() agent.Tool {
	return agent.Tool{
		Name:        "ion_live_echo",
		Description: "Return a short text marker for live provider tool-call conformance.",
		ReadOnly:    true,
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"text": map[string]any{"type": "string"},
			},
			"required": []string{"text"},
		},
		Execute: func(_ context.Context, id string, args json.RawMessage, _ <-chan struct{}, _ func(session.ToolPartial)) (session.ToolResultMessage, error) {
			var input struct {
				Text string `json:"text"`
			}
			if err := json.Unmarshal(args, &input); err != nil {
				return session.ToolResultMessage{}, fmt.Errorf("decode live echo arguments: %w", err)
			}
			if strings.TrimSpace(input.Text) == "" {
				return session.ToolResultMessage{}, fmt.Errorf("live echo text is empty")
			}
			return session.ToolResultMessage{
				ToolCallID: id,
				ToolName:   "ion_live_echo",
				Title:      "ion_live_echo",
				Content:    []session.Content{session.TextContent{Text: "live echo: " + input.Text}},
				Timestamp:  time.Now(),
			}, nil
		},
	}
}

func drainLiveEvents(sub *agent.EventSubscription) []session.Event {
	var events []session.Event
	for {
		select {
		case envelope, ok := <-sub.Events:
			if !ok {
				return events
			}
			events = append(events, envelope.Event)
		default:
			return events
		}
	}
}

func assertLiveEvents(t *testing.T, events []session.Event, profile liveProviderProfile) {
	t.Helper()
	var text, thinking, assistantStart, assistantEnd, turnEnd, agentEnd, settled bool
	var toolStarts, toolEnds int
	assistantDepth := 0
	turnEndIndex, agentEndIndex, settledIndex := -1, -1, -1
	for index, event := range events {
		switch event := event.(type) {
		case session.MessageStart:
			if _, ok := event.Message.(*session.AssistantMessage); ok {
				assistantStart = true
				assistantDepth++
			}
		case session.MessageUpdate:
			switch event.Delta.(type) {
			case session.TextDelta:
				text = true
			case session.ThinkingDelta:
				thinking = true
			}
		case session.ToolExecStart:
			if event.Name == "ion_live_echo" {
				toolStarts++
			}
		case session.ToolExecEnd:
			if event.Result.ToolName == "ion_live_echo" && !event.Result.IsError {
				toolEnds++
			}
		case session.MessageEnd:
			if _, ok := event.Message.(*session.AssistantMessage); ok {
				assistantEnd = true
				if assistantDepth == 0 {
					t.Errorf("assistant MessageEnd appeared before MessageStart")
				} else {
					assistantDepth--
				}
			}
		case session.TurnEnd:
			turnEnd = true
			turnEndIndex = index
		case session.AgentEnd:
			agentEnd = true
			agentEndIndex = index
		case session.Settled:
			settled = true
			settledIndex = index
		}
	}
	if !text {
		t.Error("live event stream contained no text delta")
	}
	if toolStarts != 1 || toolEnds != 1 {
		t.Errorf(
			"live event stream ion_live_echo count: starts=%d ends=%d, want exactly one successful call",
			toolStarts,
			toolEnds,
		)
	}
	if !settled {
		t.Error("live event stream contained no Settled event")
	}
	if !assistantStart || !assistantEnd || !turnEnd || !agentEnd {
		t.Errorf(
			"live event stream incomplete assistant lifecycle: start=%v end=%v turn_end=%v agent_end=%v",
			assistantStart,
			assistantEnd,
			turnEnd,
			agentEnd,
		)
	}
	if assistantDepth != 0 {
		t.Errorf("live event stream left %d assistant messages open", assistantDepth)
	}
	if turnEndIndex < 0 || agentEndIndex <= turnEndIndex || settledIndex <= agentEndIndex {
		t.Errorf(
			"live terminal event order = turn_end:%d agent_end:%d settled:%d; want TurnEnd < AgentEnd < Settled",
			turnEndIndex,
			agentEndIndex,
			settledIndex,
		)
	}
	if (os.Getenv("ION_LIVE_REQUIRE_THINKING") == "1" || profile.thinking != session.ThinkingAuto) && !thinking {
		t.Errorf("live event stream contained no thinking delta for profile thinking level %q", profile.thinking)
	}
}

func assertLiveDurableTurn(
	t *testing.T,
	entries []session.Entry,
	profile liveProviderProfile,
) session.AssistantMessage {
	t.Helper()
	var toolResult bool
	var assistants []session.AssistantMessage
	for _, entry := range entries {
		messageEntry, ok := entry.(*session.MessageEntry)
		if !ok {
			continue
		}
		switch message := messageEntry.Message.(type) {
		case *session.ToolResultMessage:
			if message.ToolName == "ion_live_echo" && !message.IsError &&
				strings.Contains(session.MessageText(message), "live-check") {
				toolResult = true
			}
		case *session.AssistantMessage:
			assistants = append(assistants, *message)
		}
	}
	if !toolResult {
		t.Fatal("durable entries contain no successful ion_live_echo result")
	}
	if len(assistants) < 2 {
		t.Fatalf("durable entries contain %d assistant messages, want tool-call and final responses", len(assistants))
	}
	final := assistants[len(assistants)-1]
	if final.Model != profile.model {
		t.Errorf("final assistant model = %q, want requested %q", final.Model, profile.model)
	}
	if final.Provider != profile.provider {
		t.Errorf("final assistant provider = %q, want %q", final.Provider, profile.provider)
	}
	if final.ResponseID == "" {
		t.Error("final assistant has no provider response identity")
	}
	if final.Usage.TotalTokens <= 0 {
		t.Errorf("final assistant usage total = %d, want provider usage metadata", final.Usage.TotalTokens)
	}
	if final.StopReason == "" || final.StopReason == session.StopReasonError ||
		final.StopReason == session.StopReasonAborted {
		t.Errorf("final assistant stop reason = %q, want a successful terminal reason", final.StopReason)
	}
	return final
}

func containsLiveText(messages []session.Message, want string) bool {
	want = strings.TrimSpace(want)
	if want == "" {
		return false
	}
	for _, message := range messages {
		if strings.Contains(session.MessageText(message), want) {
			return true
		}
	}
	return false
}

func liveMessageSummary(messages []session.Message) string {
	parts := make([]string, 0, len(messages))
	for _, message := range messages {
		parts = append(parts, fmt.Sprintf("%T:%q", message, session.MessageText(message)))
	}
	return strings.Join(parts, ", ")
}
