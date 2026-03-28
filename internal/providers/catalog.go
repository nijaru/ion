package providers

import (
	"fmt"
	"os"
	"slices"
	"strings"

	"github.com/nijaru/ion/internal/config"
)

type Kind string
type Family string
type AuthKind string
type Runtime string

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

func MustLookup(id string) Definition {
	def, ok := Lookup(id)
	if !ok {
		panic("unknown provider: " + id)
	}
	return def
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
	if cfg == nil {
		return ""
	}
	if endpoint := strings.TrimSpace(cfg.Endpoint); endpoint != "" {
		return endpoint
	}
	def, ok := Lookup(cfg.Provider)
	if !ok {
		return ""
	}
	return strings.TrimSpace(def.DefaultEndpoint)
}

func ResolvedAuthEnvVar(cfg *config.Config) string {
	if cfg == nil {
		return ""
	}
	if envVar := strings.TrimSpace(cfg.AuthEnvVar); envVar != "" {
		return envVar
	}
	def, ok := Lookup(cfg.Provider)
	if !ok {
		return ""
	}
	return def.DefaultEnvVar
}

func ResolvedHeaders(cfg *config.Config) map[string]string {
	def, ok := Lookup(cfg.Provider)
	if !ok {
		return cloneHeaders(cfg.ExtraHeaders)
	}
	headers := cloneHeaders(def.DefaultHeaders)
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
	return headers
}

func CredentialState(cfg *config.Config, def Definition) (string, bool) {
	if def.Runtime == RuntimeACP {
		return "Subscription", true
	}
	if def.AuthKind == AuthLocal {
		return "Ready", true
	}
	if def.Kind == KindCustom && def.DefaultEndpoint == "" && strings.TrimSpace(cfg.Endpoint) == "" {
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
	case !ready && !isLocal:
		return 2
	default:
		return 3
	}
}

func RequiresEndpoint(cfg *config.Config) bool {
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
	if def.Kind != KindCustom {
		return true
	}
	if cfg == nil {
		return false
	}
	return ResolveID(cfg.Provider) == def.ID || strings.TrimSpace(cfg.Endpoint) != ""
}

func SupportsModelListing(cfg *config.Config) bool {
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
	if override := strings.TrimSpace(cfg.AuthEnvVar); override != "" {
		return []string{override}
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

var definitions = []Definition{
	{ID: "anthropic", DisplayName: "Anthropic", Kind: KindDirect, Family: FamilyAnthropic, AuthKind: AuthAPIKey, DefaultEnvVar: "ANTHROPIC_API_KEY", SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "openai", DisplayName: "OpenAI", Kind: KindDirect, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "OPENAI_API_KEY", DefaultEndpoint: "https://api.openai.com/v1", SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "openrouter", DisplayName: "OpenRouter", Kind: KindRouter, Family: FamilyOpenRouter, AuthKind: AuthAPIKey, DefaultEnvVar: "OPENROUTER_API_KEY", DefaultEndpoint: "https://openrouter.ai/api/v1", SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "gemini", DisplayName: "Gemini", Kind: KindDirect, Family: FamilyGemini, AuthKind: AuthAPIKey, DefaultEnvVar: "GEMINI_API_KEY", AlternateEnvVars: []string{"GOOGLE_API_KEY"}, SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "ollama", DisplayName: "Ollama", Kind: KindLocal, Family: FamilyOllama, AuthKind: AuthLocal, DefaultEndpoint: "http://localhost:11434/v1", SupportsModelListing: true, SupportsCustomEndpoint: true, Runtime: RuntimeNative},
	{ID: "huggingface", DisplayName: "Hugging Face", Kind: KindRouter, Family: FamilyOpenAI, AuthKind: AuthToken, DefaultEnvVar: "HF_TOKEN", DefaultEndpoint: "https://router.huggingface.co/v1", SupportsModelListing: false, Runtime: RuntimeNative},
	{ID: "together", DisplayName: "Together AI", Kind: KindRouter, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "TOGETHER_API_KEY", DefaultEndpoint: "https://api.together.xyz/v1", SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "deepseek", DisplayName: "DeepSeek", Kind: KindDirect, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "DEEPSEEK_API_KEY", DefaultEndpoint: "https://api.deepseek.com/v1", SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "groq", DisplayName: "Groq", Kind: KindDirect, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "GROQ_API_KEY", DefaultEndpoint: "https://api.groq.com/openai/v1", SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "fireworks", DisplayName: "Fireworks AI", Kind: KindDirect, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "FIREWORKS_API_KEY", DefaultEndpoint: "https://api.fireworks.ai/inference/v1", SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "mistral", DisplayName: "Mistral", Kind: KindDirect, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "MISTRAL_API_KEY", DefaultEndpoint: "https://api.mistral.ai/v1", SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "xai", DisplayName: "xAI", Kind: KindDirect, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "XAI_API_KEY", DefaultEndpoint: "https://api.x.ai/v1", SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "moonshot", DisplayName: "Moonshot AI", Kind: KindDirect, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "MOONSHOT_API_KEY", DefaultEndpoint: "https://api.moonshot.ai/v1", SupportsModelListing: false, Runtime: RuntimeNative},
	{ID: "cerebras", DisplayName: "Cerebras", Kind: KindDirect, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "CEREBRAS_API_KEY", DefaultEndpoint: "https://api.cerebras.ai/v1", SupportsModelListing: true, Runtime: RuntimeNative},
	{ID: "zai", DisplayName: "Z.ai", Kind: KindDirect, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "ZAI_API_KEY", SupportsModelListing: false, Runtime: RuntimeNative, Aliases: []string{"z-ai"}},
	{ID: "openai-compatible", DisplayName: "Custom OpenAI", Kind: KindCustom, Family: FamilyOpenAI, AuthKind: AuthAPIKey, DefaultEnvVar: "OPENAI_COMPATIBLE_API_KEY", SupportsModelListing: true, SupportsCustomEndpoint: true, Runtime: RuntimeNative},
	{ID: "local-openai", DisplayName: "Local OpenAI", Kind: KindLocal, Family: FamilyOpenAI, AuthKind: AuthLocal, DefaultEndpoint: "http://127.0.0.1:1234/v1", SupportsModelListing: true, SupportsCustomEndpoint: true, Runtime: RuntimeNative},
	{ID: "claude-pro", DisplayName: "Claude Code", Kind: KindSubscription, Family: FamilyACP, AuthKind: AuthACP, Runtime: RuntimeACP, ACPCommand: "claude --acp"},
	{ID: "gemini-advanced", DisplayName: "Gemini CLI", Kind: KindSubscription, Family: FamilyACP, AuthKind: AuthACP, Runtime: RuntimeACP, ACPCommand: "gemini --acp"},
	{ID: "gh-copilot", DisplayName: "GitHub Copilot", Kind: KindSubscription, Family: FamilyACP, AuthKind: AuthACP, Runtime: RuntimeACP, ACPCommand: "gh copilot --acp"},
	{ID: "chatgpt", DisplayName: "ChatGPT", Kind: KindSubscription, Family: FamilyACP, AuthKind: AuthACP, Runtime: RuntimeACP, ACPCommand: "codex --acp"},
	{ID: "codex", DisplayName: "Codex CLI", Kind: KindSubscription, Family: FamilyACP, AuthKind: AuthACP, Runtime: RuntimeACP, ACPCommand: "codex --acp"},
}
