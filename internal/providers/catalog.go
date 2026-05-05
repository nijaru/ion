package providers

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/nijaru/ion/internal/config"
)

type (
	Kind     string
	Family   string
	AuthKind string
	Runtime  string
)

const (
	KindDirect       Kind = "direct"
	KindRouter       Kind = "router"
	KindLocal        Kind = "local"
	KindCustom       Kind = "custom"
	KindSubscription Kind = "subscription"
)

const (
	FamilyOpenAI     Family = "openai"
	FamilyAnthropic  Family = "anthropic"
	FamilyGemini     Family = "gemini"
	FamilyOpenRouter Family = "openrouter"
	FamilyOllama     Family = "ollama"
	FamilyACP        Family = "acp"
)

const (
	AuthAPIKey AuthKind = "api_key"
	AuthToken  AuthKind = "token"
	AuthLocal  AuthKind = "local"
	AuthACP    AuthKind = "acp"
)

const (
	RuntimeNative Runtime = "native"
	RuntimeACP    Runtime = "acp"
)

type Definition struct {
	ID                     string
	DisplayName            string
	Kind                   Kind
	Family                 Family
	AuthKind               AuthKind
	DefaultEnvVar          string
	AlternateEnvVars       []string
	DefaultEndpoint        string
	SupportsModelListing   bool
	SupportsCustomEndpoint bool
	Runtime                Runtime
	DefaultHeaders         map[string]string
	Aliases                []string
	ACPCommand             string
}

type localProbeResult struct {
	endpoint string
	ready    bool
	checked  time.Time
}

var (
	localProbeMu    sync.RWMutex
	localProbeCache = map[string]localProbeResult{}
)

const (
	localProbeTTL     = 5 * time.Second
	localProbeTimeout = 300 * time.Millisecond
)

func All() []Definition {
	return slices.Clone(definitions)
}

func Native() []Definition {
	out := make([]Definition, 0, len(definitions))
	for _, def := range definitions {
		if def.Runtime == RuntimeNative {
			out = append(out, def)
		}
	}
	return out
}

func Lookup(id string) (Definition, bool) {
	needle := normalize(id)
	for _, def := range definitions {
		if normalize(def.ID) == needle {
			return def, true
		}
		for _, alias := range def.Aliases {
			if normalize(alias) == needle {
				return def, true
			}
		}
	}
	return Definition{}, false
}

func ResolveID(id string) string {
	if def, ok := Lookup(id); ok {
		return def.ID
	}
	return normalize(id)
}

func DisplayName(id string) string {
	if def, ok := Lookup(id); ok {
		return def.DisplayName
	}
	return id
}

func IsACP(id string) bool {
	def, ok := Lookup(id)
	return ok && def.Runtime == RuntimeACP
}

func DefaultACPCommand(id string) (string, bool) {
	def, ok := Lookup(id)
	if !ok || def.ACPCommand == "" {
		return "", false
	}
	return def.ACPCommand, true
}

func ResolvedEndpoint(cfg *config.Config) string {
	return ResolvedEndpointContext(context.Background(), cfg)
}

func ResolvedEndpointContext(ctx context.Context, cfg *config.Config) string {
	if cfg == nil {
		return ""
	}
	def, ok := Lookup(cfg.Provider)
	if !ok {
		return ""
	}
	if endpoint := strings.TrimSpace(cfg.Endpoint); endpoint != "" && def.SupportsCustomEndpoint {
		return endpoint
	}
	if def.ID == "local-api" {
		if endpoint, ok := ProbeLocalAPI(ctx, cfg); ok {
			return endpoint
		}
		return ""
	}
	return strings.TrimSpace(def.DefaultEndpoint)
}

func ResolvedAuthEnvVar(cfg *config.Config) string {
	if cfg == nil {
		return ""
	}
	def, ok := Lookup(cfg.Provider)
	if !ok {
		return ""
	}
	if envVar := strings.TrimSpace(cfg.AuthEnvVar); envVar != "" && def.SupportsCustomEndpoint {
		return envVar
	}
	return def.DefaultEnvVar
}

func CredentialEnvVars(cfg *config.Config) []string {
	seen := map[string]struct{}{}
	add := func(value string) {
		envVar := strings.TrimSpace(value)
		if envVar != "" {
			seen[envVar] = struct{}{}
		}
	}
	if cfg != nil {
		add(cfg.AuthEnvVar)
	}
	for _, def := range definitions {
		add(def.DefaultEnvVar)
		for _, envVar := range def.AlternateEnvVars {
			add(envVar)
		}
	}
	out := make([]string, 0, len(seen))
	for envVar := range seen {
		out = append(out, envVar)
	}
	slices.Sort(out)
	return out
}

func ResolvedHeaders(cfg *config.Config) map[string]string {
	if cfg == nil {
		return nil
	}
	def, ok := Lookup(cfg.Provider)
	if !ok {
		return cloneHeaders(cfg.ExtraHeaders)
	}
	headers := cloneHeaders(def.DefaultHeaders)
	if def.SupportsCustomEndpoint {
		for k, v := range cfg.ExtraHeaders {
			key := strings.TrimSpace(k)
			value := strings.TrimSpace(v)
			if key == "" || value == "" {
				continue
			}
			if headers == nil {
				headers = make(map[string]string)
			}
			headers[key] = value
		}
	}
	return headers
}

func CredentialState(cfg *config.Config, def Definition) (string, bool) {
	return CredentialStateContext(context.Background(), cfg, def)
}

func CredentialStateContext(
	ctx context.Context,
	cfg *config.Config,
	def Definition,
) (string, bool) {
	if def.Runtime == RuntimeACP {
		return "Subscription", true
	}
	if def.ID == "local-api" {
		if endpoint, ok := ProbeLocalAPI(ctx, cfg); ok {
			return "Ready at " + summarizeEndpoint(endpoint), true
		}
		return "Not running", false
	}
	if def.AuthKind == AuthLocal {
		return "Ready", true
	}
	endpoint := ""
	if cfg != nil {
		endpoint = cfg.Endpoint
	}
	if def.Kind == KindCustom && def.DefaultEndpoint == "" &&
		strings.TrimSpace(endpoint) == "" {
		return "Set endpoint", false
	}
	for _, envVar := range authEnvVars(cfg, def) {
		if strings.TrimSpace(envVar) == "" {
			continue
		}
		if strings.TrimSpace(os.Getenv(envVar)) != "" {
			return "Ready", true
		}
	}
	if def.AuthKind == AuthLocal {
		return "Local", true
	}
	if envVar := ResolvedAuthEnvVar(cfg); envVar != "" {
		return fmt.Sprintf("Set %s", envVar), false
	}
	if def.DefaultEnvVar != "" {
		return fmt.Sprintf("Set %s", def.DefaultEnvVar), false
	}
	return "Set provider options", false
}

func GroupName(def Definition) string {
	switch def.Kind {
	case KindDirect:
		return "Direct APIs"
	case KindRouter:
		return "Routers"
	case KindLocal:
		return "Local"
	case KindCustom:
		return "Custom Endpoints"
	default:
		return ""
	}
}

func SortRank(cfg *config.Config, def Definition) int {
	_, ready := CredentialState(cfg, def)
	isLocal := def.Kind == KindLocal
	switch {
	case ready && !isLocal:
		return 0
	case ready && isLocal:
		return 1
	case !ready && isLocal:
		return 2
	default:
		return 3
	}
}

func RequiresEndpoint(cfg *config.Config) bool {
	if cfg == nil {
		return false
	}
	def, ok := Lookup(cfg.Provider)
	if !ok {
		return false
	}
	if def.DefaultEndpoint != "" {
		return false
	}
	return def.SupportsCustomEndpoint
}

func ShowInPicker(cfg *config.Config, def Definition) bool {
	if def.Runtime != RuntimeNative {
		return false
	}
	if def.ID == "local-api" {
		return true
	}
	if def.Kind != KindCustom {
		return true
	}
	if cfg == nil {
		return false
	}
	return ResolveID(cfg.Provider) == def.ID
}

func SupportsModelListing(cfg *config.Config) bool {
	if cfg == nil {
		return false
	}
	def, ok := Lookup(cfg.Provider)
	if !ok {
		return false
	}
	if def.SupportsModelListing {
		return true
	}
	return false
}

func authEnvVars(cfg *config.Config, def Definition) []string {
	if cfg != nil {
		if override := strings.TrimSpace(cfg.AuthEnvVar); override != "" {
			return []string{override}
		}
	}
	fields := make([]string, 0, 1+len(def.AlternateEnvVars))
	if def.DefaultEnvVar != "" {
		fields = append(fields, def.DefaultEnvVar)
	}
	fields = append(fields, def.AlternateEnvVars...)
	return fields
}

func normalize(value string) string {
	return strings.ToLower(strings.TrimSpace(value))
}

func cloneHeaders(src map[string]string) map[string]string {
	if len(src) == 0 {
		return nil
	}
	dst := make(map[string]string, len(src))
	for k, v := range src {
		dst[k] = v
	}
	return dst
}

func summarizeEndpoint(raw string) string {
	value := strings.TrimSpace(raw)
	if value == "" {
		return "127.0.0.1:1234"
	}
	u, err := url.Parse(value)
	if err != nil || u.Host == "" {
		return value
	}
	return u.Host
}

func ProbeLocalAPI(ctx context.Context, cfg *config.Config) (string, bool) {
	for _, endpoint := range localAPIProbeTargets(cfg) {
		if endpoint == "" {
			continue
		}
		if cached, ok := localProbeCached(endpoint); ok {
			if cached.ready {
				return cached.endpoint, true
			}
			continue
		}
		ready := probeOpenAICompatibleEndpoint(ctx, endpoint)
		storeLocalProbe(endpoint, ready)
		if ready {
			return endpoint, true
		}
	}
	return "", false
}

func localAPIProbeTargets(cfg *config.Config) []string {
	seen := map[string]struct{}{}
	out := make([]string, 0, 4)
	add := func(raw string) {
		value := strings.TrimSpace(raw)
		if value == "" {
			return
		}
		value = strings.TrimRight(value, "/")
		if _, ok := seen[value]; ok {
			return
		}
		seen[value] = struct{}{}
		out = append(out, value)
	}

	if cfg != nil && strings.EqualFold(strings.TrimSpace(cfg.Provider), "local-api") {
		add(cfg.Endpoint)
	}
	add("http://127.0.0.1:1234/v1")
	add("http://127.0.0.1:8000/v1")
	add("http://127.0.0.1:8080/v1")
	return out
}

func localProbeCached(endpoint string) (localProbeResult, bool) {
	localProbeMu.RLock()
	defer localProbeMu.RUnlock()
	result, ok := localProbeCache[endpoint]
	if !ok {
		return localProbeResult{}, false
	}
	if time.Since(result.checked) > localProbeTTL {
		return localProbeResult{}, false
	}
	return result, true
}

func storeLocalProbe(endpoint string, ready bool) {
	localProbeMu.Lock()
	defer localProbeMu.Unlock()
	localProbeCache[endpoint] = localProbeResult{
		endpoint: endpoint,
		ready:    ready,
		checked:  time.Now(),
	}
}

func probeOpenAICompatibleEndpoint(ctx context.Context, endpoint string) bool {
	reqCtx := ctx
	if _, ok := reqCtx.Deadline(); !ok {
		var cancel context.CancelFunc
		reqCtx, cancel = context.WithTimeout(reqCtx, localProbeTimeout)
		defer cancel()
	}
	req, err := http.NewRequestWithContext(
		reqCtx,
		http.MethodGet,
		strings.TrimRight(endpoint, "/")+"/models",
		nil,
	)
	if err != nil {
		return false
	}
	req.Header.Set("User-Agent", "ion/0.0.0")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return false
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return false
	}
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return false
	}
	var payload struct {
		Data []struct {
			ID string `json:"id"`
		} `json:"data"`
	}
	return json.Unmarshal(body, &payload) == nil
}

var definitions = []Definition{
	{
		ID:                   "anthropic",
		DisplayName:          "Anthropic",
		Kind:                 KindDirect,
		Family:               FamilyAnthropic,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "ANTHROPIC_API_KEY",
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "openai",
		DisplayName:          "OpenAI",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "OPENAI_API_KEY",
		DefaultEndpoint:      "https://api.openai.com/v1",
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "openrouter",
		DisplayName:          "OpenRouter",
		Kind:                 KindRouter,
		Family:               FamilyOpenRouter,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "OPENROUTER_API_KEY",
		DefaultEndpoint:      "https://openrouter.ai/api/v1",
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "gemini",
		DisplayName:          "Gemini",
		Kind:                 KindDirect,
		Family:               FamilyGemini,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "GEMINI_API_KEY",
		AlternateEnvVars:     []string{"GOOGLE_API_KEY"},
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                     "ollama",
		DisplayName:            "Ollama",
		Kind:                   KindLocal,
		Family:                 FamilyOllama,
		AuthKind:               AuthLocal,
		DefaultEndpoint:        "http://localhost:11434/v1",
		SupportsModelListing:   true,
		SupportsCustomEndpoint: true,
		Runtime:                RuntimeNative,
	},
	{
		ID:                   "huggingface",
		DisplayName:          "Hugging Face",
		Kind:                 KindRouter,
		Family:               FamilyOpenAI,
		AuthKind:             AuthToken,
		DefaultEnvVar:        "HF_TOKEN",
		DefaultEndpoint:      "https://router.huggingface.co/v1",
		SupportsModelListing: false,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "together",
		DisplayName:          "Together AI",
		Kind:                 KindRouter,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "TOGETHER_API_KEY",
		DefaultEndpoint:      "https://api.together.xyz/v1",
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "deepseek",
		DisplayName:          "DeepSeek",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "DEEPSEEK_API_KEY",
		DefaultEndpoint:      "https://api.deepseek.com/v1",
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "groq",
		DisplayName:          "Groq",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "GROQ_API_KEY",
		DefaultEndpoint:      "https://api.groq.com/openai/v1",
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "fireworks",
		DisplayName:          "Fireworks AI",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "FIREWORKS_API_KEY",
		DefaultEndpoint:      "https://api.fireworks.ai/inference/v1",
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "mistral",
		DisplayName:          "Mistral",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "MISTRAL_API_KEY",
		DefaultEndpoint:      "https://api.mistral.ai/v1",
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "xai",
		DisplayName:          "xAI",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "XAI_API_KEY",
		DefaultEndpoint:      "https://api.x.ai/v1",
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "moonshot",
		DisplayName:          "Moonshot AI",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "MOONSHOT_API_KEY",
		DefaultEndpoint:      "https://api.moonshot.ai/v1",
		SupportsModelListing: false,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "cerebras",
		DisplayName:          "Cerebras",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "CEREBRAS_API_KEY",
		DefaultEndpoint:      "https://api.cerebras.ai/v1",
		SupportsModelListing: true,
		Runtime:              RuntimeNative,
	},
	{
		ID:                   "zai",
		DisplayName:          "Z.ai",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "ZAI_API_KEY",
		DefaultEndpoint:      "https://api.z.ai/api/paas/v4",
		SupportsModelListing: false,
		Runtime:              RuntimeNative,
		Aliases:              []string{"z-ai"},
	},
	{
		ID:                     "openai-compatible",
		DisplayName:            "Custom API",
		Kind:                   KindCustom,
		Family:                 FamilyOpenAI,
		AuthKind:               AuthAPIKey,
		DefaultEnvVar:          "OPENAI_COMPATIBLE_API_KEY",
		SupportsModelListing:   true,
		SupportsCustomEndpoint: true,
		Runtime:                RuntimeNative,
	},
	{
		ID:                     "local-api",
		DisplayName:            "Local API",
		Kind:                   KindLocal,
		Family:                 FamilyOpenAI,
		AuthKind:               AuthLocal,
		SupportsModelListing:   true,
		SupportsCustomEndpoint: true,
		Runtime:                RuntimeNative,
	},
	{
		ID:          "claude-pro",
		DisplayName: "Claude Code",
		Kind:        KindSubscription,
		Family:      FamilyACP,
		AuthKind:    AuthACP,
		Runtime:     RuntimeACP,
		ACPCommand:  "claude --acp",
	},
	{
		ID:          "gemini-advanced",
		DisplayName: "Gemini CLI",
		Kind:        KindSubscription,
		Family:      FamilyACP,
		AuthKind:    AuthACP,
		Runtime:     RuntimeACP,
		ACPCommand:  "gemini --acp",
	},
	{
		ID:          "gh-copilot",
		DisplayName: "GitHub Copilot",
		Kind:        KindSubscription,
		Family:      FamilyACP,
		AuthKind:    AuthACP,
		Runtime:     RuntimeACP,
		ACPCommand:  "gh copilot --acp",
	},
	{
		ID:          "chatgpt",
		DisplayName: "ChatGPT",
		Kind:        KindSubscription,
		Family:      FamilyACP,
		AuthKind:    AuthACP,
		Runtime:     RuntimeACP,
	},
	{
		ID:          "codex",
		DisplayName: "Codex CLI",
		Kind:        KindSubscription,
		Family:      FamilyACP,
		AuthKind:    AuthACP,
		Runtime:     RuntimeACP,
	},
}
