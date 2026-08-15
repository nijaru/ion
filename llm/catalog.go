package llm

import (
	"context"
	"fmt"
	"net/url"
	"os"
	"slices"
	"strings"

	"github.com/nijaru/ion/config"
)

type (
	Kind     string
	Family   string
	AuthKind string
)

const (
	KindDirect Kind = "direct"
	KindRouter Kind = "router"
	KindLocal  Kind = "local"
	KindCustom Kind = "custom"
)

const (
	FamilyOpenAI     Family = "openai"
	FamilyAnthropic  Family = "anthropic"
	FamilyGemini     Family = "gemini"
	FamilyOpenRouter Family = "openrouter"
	FamilyOllama     Family = "ollama"
)

const (
	AuthAPIKey   AuthKind = "api_key"
	AuthToken    AuthKind = "token"
	AuthLocal    AuthKind = "local"
	AuthOptional AuthKind = "optional"
)

const OpenAICompatibleID = "openai-compatible"

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
	DefaultHeaders         map[string]string
	Aliases                []string
}

func All() []Definition {
	return slices.Clone(definitions)
}

func Native() []Definition {
	return All()
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

func LookupConfig(cfg *config.Config, id string) (Definition, bool) {
	if def, ok := Lookup(id); ok {
		return def, true
	}
	needle := normalize(id)
	if needle == "" {
		return Definition{}, false
	}
	if cfg != nil {
		if cfg.Providers != nil {
			for name, ps := range cfg.Providers {
				if normalize(name) == needle {
					family := FamilyOpenAI
					if strings.EqualFold(ps.Family, "anthropic") {
						family = FamilyAnthropic
					}
					displayName := ps.DisplayName
					if displayName == "" {
						displayName = name
					}
					authKind := AuthOptional
					if strings.TrimSpace(ps.APIKey) != "" || strings.TrimSpace(ps.AuthEnvVar) != "" {
						authKind = AuthAPIKey
					}
					return Definition{
						ID:                     normalize(name),
						DisplayName:            displayName,
						Kind:                   KindCustom,
						Family:                 family,
						AuthKind:               authKind,
						DefaultEnvVar:          ps.AuthEnvVar,
						DefaultEndpoint:        ps.Endpoint,
						SupportsModelListing:   true,
						SupportsCustomEndpoint: true,
					}, true
				}
			}
		}
		if (normalize(cfg.Provider) == needle || cfg.Endpoint != "") && strings.TrimSpace(cfg.Endpoint) != "" {
			displayName := id
			if normalize(cfg.Provider) == needle && strings.TrimSpace(cfg.Provider) != "" {
				displayName = cfg.Provider
			}
			return Definition{
				ID:                     needle,
				DisplayName:            displayName,
				Kind:                   KindCustom,
				Family:                 FamilyOpenAI,
				AuthKind:               AuthOptional,
				DefaultEnvVar:          cfg.AuthEnvVar,
				DefaultEndpoint:        cfg.Endpoint,
				SupportsModelListing:   true,
				SupportsCustomEndpoint: true,
			}, true
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

func ResolveIDConfig(cfg *config.Config, id string) string {
	if def, ok := LookupConfig(cfg, id); ok {
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

func DisplayNameConfig(cfg *config.Config, id string) string {
	if def, ok := LookupConfig(cfg, id); ok {
		return def.DisplayName
	}
	return id
}

func IsOpenAICompatible(id string) bool {
	return ResolveID(id) == OpenAICompatibleID
}

func IsOpenAICompatibleConfig(cfg *config.Config, id string) bool {
	def, ok := LookupConfig(cfg, id)
	if !ok {
		return false
	}
	return def.ID == OpenAICompatibleID || (def.Kind == KindCustom && def.Family == FamilyOpenAI)
}

func ResolvedAuthEnvVar(cfg *config.Config) string {
	if cfg == nil {
		return ""
	}
	def, ok := LookupConfig(cfg, cfg.Provider)
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
		if cfg.Providers != nil {
			for _, ps := range cfg.Providers {
				add(ps.AuthEnvVar)
			}
		}
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

func ResolvedAuthToken(cfg *config.Config, def Definition) string {
	if def.AuthKind == AuthLocal {
		return ""
	}
	if cfg != nil {
		if override := strings.TrimSpace(cfg.APIKeyOverride); override != "" &&
			ResolveIDConfig(cfg, cfg.Provider) == ResolveIDConfig(cfg, cfg.APIKeyOverrideProvider) {
			return override
		}
		if cfg.Providers != nil {
			if ps, ok := cfg.Providers[def.ID]; ok && strings.TrimSpace(ps.APIKey) != "" {
				return strings.TrimSpace(ps.APIKey)
			}
		}
	}
	for _, envVar := range authEnvVars(cfg, def) {
		if value := strings.TrimSpace(os.Getenv(envVar)); value != "" {
			return value
		}
	}
	if key, ok := config.LookupAPIKey(def.ID); ok {
		return key
	}
	return ""
}

func RequiresAuth(cfg *config.Config, def Definition) bool {
	switch def.AuthKind {
	case AuthAPIKey, AuthToken:
		return true
	case AuthOptional:
		return cfg != nil && strings.TrimSpace(cfg.AuthEnvVar) != ""
	default:
		return false
	}
}

func MissingAuthDetail(cfg *config.Config, def Definition) string {
	if def.SupportsCustomEndpoint && cfg != nil {
		if override := strings.TrimSpace(cfg.AuthEnvVar); override != "" {
			return override
		}
	}
	if envVar := ResolvedAuthEnvVar(cfg); envVar != "" {
		return envVar
	}
	if def.DefaultEnvVar != "" {
		return def.DefaultEnvVar
	}
	return "provider credentials"
}

func ResolvedHeaders(cfg *config.Config) map[string]string {
	if cfg == nil {
		return nil
	}
	def, ok := LookupConfig(cfg, cfg.Provider)
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

func CredentialStateContext(
	ctx context.Context,
	cfg *config.Config,
	def Definition,
	resolver *EndpointResolver,
) (string, bool) {
	if def.ID == OpenAICompatibleID || (def.Kind == KindCustom && def.Family == FamilyOpenAI) {
		configuredEndpoint := ""
		if cfg != nil {
			if cfg.Providers != nil {
				if ps, ok := cfg.Providers[def.ID]; ok && strings.TrimSpace(ps.Endpoint) != "" {
					configuredEndpoint = strings.TrimSpace(ps.Endpoint)
				}
			}
			if configuredEndpoint == "" {
				configuredEndpoint = strings.TrimSpace(cfg.Endpoint)
			}
		}
		if configuredEndpoint == "" {
			configuredEndpoint = strings.TrimSpace(def.DefaultEndpoint)
		}
		if RequiresAuth(cfg, def) && ResolvedAuthToken(cfg, def) == "" {
			return fmt.Sprintf("Set %s", MissingAuthDetail(cfg, def)), false
		}
		if resolver != nil {
			if endpoint, ok := resolver.Probe(ctx, cfg); ok {
				return "Ready at " + summarizeEndpoint(endpoint), true
			}
		}
		if configuredEndpoint == "" {
			return "Set endpoint", false
		}
		return "Not running", false
	}
	if def.AuthKind == AuthLocal {
		return "Ready", true
	}
	endpoint := ""
	if cfg != nil {
		if cfg.Providers != nil {
			if ps, ok := cfg.Providers[def.ID]; ok && strings.TrimSpace(ps.Endpoint) != "" {
				endpoint = strings.TrimSpace(ps.Endpoint)
			}
		}
		if endpoint == "" {
			endpoint = cfg.Endpoint
		}
	}
	if def.Kind == KindCustom && def.DefaultEndpoint == "" &&
		strings.TrimSpace(endpoint) == "" {
		return "Set endpoint", false
	}
	if ResolvedAuthToken(cfg, def) != "" {
		return "Ready", true
	}
	if def.AuthKind == AuthLocal || def.AuthKind == AuthOptional {
		return "Local", true
	}
	if RequiresAuth(cfg, def) {
		return fmt.Sprintf("Set %s", MissingAuthDetail(cfg, def)), false
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
		return "Local / custom"
	default:
		return ""
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
	if def.ID == OpenAICompatibleID {
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

func authEnvVars(cfg *config.Config, def Definition) []string {
	if cfg != nil && def.SupportsCustomEndpoint {
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

func EndpointDisplayName(raw string) string {
	value := strings.TrimSpace(raw)
	if value == "" {
		return ""
	}
	return summarizeEndpoint(value)
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
		Aliases:              []string{"z-ai"},
	},
	{
		ID:                     "openai-compatible",
		DisplayName:            "OpenAI-compatible",
		Kind:                   KindCustom,
		Family:                 FamilyOpenAI,
		AuthKind:               AuthOptional,
		DefaultEnvVar:          "OPENAI_COMPATIBLE_API_KEY",
		SupportsModelListing:   true,
		SupportsCustomEndpoint: true,
		Aliases:                []string{"local-api", "custom-api"},
	},
	// Vercel AI Gateway
	{
		ID:                   "vercel-ai-gateway",
		DisplayName:          "Vercel AI Gateway",
		Kind:                 KindRouter,
		Family:               FamilyOpenAI,
		AuthKind:             AuthToken,
		DefaultEnvVar:        "VERCEL_AI_GATEWAY_TOKEN",
		DefaultEndpoint:      "https://ai-gateway.vercel.sh/v1",
		SupportsModelListing: true,
	},

	// Xiaomi Token Plan (China)
	{
		ID:                   "xiaomi-token-plan-cn",
		DisplayName:          "Xiaomi Token Plan (CN)",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "XIAOMI_API_KEY",
		DefaultEndpoint:      "https://api.xiaomimimo.com/v1",
		SupportsModelListing: true,
	},

	// Xiaomi Token Plan (Amsterdam)
	{
		ID:                   "xiaomi-token-plan-ams",
		DisplayName:          "Xiaomi Token Plan (AMS)",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "XIAOMI_API_KEY",
		DefaultEndpoint:      "https://api-ams.xiaomimimo.com/v1",
		SupportsModelListing: true,
	},

	// Xiaomi Token Plan (Singapore)
	{
		ID:                   "xiaomi-token-plan-sgp",
		DisplayName:          "Xiaomi Token Plan (SGP)",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "XIAOMI_API_KEY",
		DefaultEndpoint:      "https://api-sgp.xiaomimimo.com/v1",
		SupportsModelListing: true,
	},

	// Moonshot AI (China)
	{
		ID:                   "moonshotai-cn",
		DisplayName:          "Moonshot AI (CN)",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "MOONSHOT_API_KEY",
		DefaultEndpoint:      "https://api.moonshot.cn/v1",
		SupportsModelListing: true,
	},

	// Kimi Coding
	{
		ID:                   "kimi-coding",
		DisplayName:          "Kimi Coding",
		Kind:                 KindDirect,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "KIMI_CODING_API_KEY",
		DefaultEndpoint:      "https://api.kimi.com/v1",
		SupportsModelListing: true,
	},

	// OpenCode
	{
		ID:                   "opencode",
		DisplayName:          "OpenCode",
		Kind:                 KindRouter,
		Family:               FamilyOpenAI,
		AuthKind:             AuthAPIKey,
		DefaultEnvVar:        "OPENCODE_API_KEY",
		DefaultEndpoint:      "https://opencode.ai/v1",
		SupportsModelListing: true,
	},
}
