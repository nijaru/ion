package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

const (
	DefaultSessionRetentionDays = 90
	defaultModelCacheTTLSeconds = 3600
	DefaultReasoningEffort      = "auto"
)

type Config struct {
	Provider               string                    `toml:"provider,omitempty"`
	Model                  string                    `toml:"model,omitempty"`
	ReasoningEffort        string                    `toml:"reasoning_effort,omitempty"`
	FastModel              string                    `toml:"fast_model,omitempty"`
	FastReasoningEffort    string                    `toml:"fast_reasoning_effort,omitempty"`
	SummaryModel           string                    `toml:"summary_model,omitempty"`
	SummaryReasoningEffort string                    `toml:"summary_reasoning_effort,omitempty"`
	Endpoint               string                    `toml:"endpoint,omitempty"`
	AuthEnvVar             string                    `toml:"auth_env_var,omitempty"`
	ExtraHeaders           map[string]string         `toml:"extra_headers,omitempty"`
	ContextLimit           int                       `toml:"context_limit,omitempty"`
	MaxSessionCost         float64                   `toml:"max_session_cost,omitempty"`
	MaxTurnCost            float64                   `toml:"max_turn_cost,omitempty"`
	RetryUntilCancelled    *bool                     `toml:"retry_until_cancelled,omitempty"`
	TelemetryOTLPEndpoint  string                    `toml:"telemetry_otlp_endpoint,omitempty"`
	TelemetryOTLPInsecure  bool                      `toml:"telemetry_otlp_insecure,omitempty"`
	TelemetryOTLPHeaders   map[string]string         `toml:"telemetry_otlp_headers,omitempty"`
	SubagentsPath          string                    `toml:"subagents_path,omitempty"`
	SessionRetentionDays   int                       `toml:"session_retention_days,omitempty"`
	ToolVerbosity          string                    `toml:"tool_verbosity,omitempty"`
	ReadOutput             string                    `toml:"read_output,omitempty"`
	WriteOutput            string                    `toml:"write_output,omitempty"`
	BashOutput             string                    `toml:"bash_output,omitempty"`
	ThinkingVerbosity      string                    `toml:"thinking_verbosity,omitempty"`
	BusyInput              string                    `toml:"busy_input,omitempty"`
	SkillTools             string                    `toml:"skill_tools,omitempty"`
	SubagentTools          string                    `toml:"subagent_tools,omitempty"`
	ToolMode               string                    `toml:"tool_mode,omitempty"`
	ToolEnv                string                    `toml:"tool_env,omitempty"`
	ModelCapabilities      []ModelCapabilityOverride `toml:"model_capabilities,omitempty"`
}

type ModelCapabilityOverride struct {
	Pattern       string `toml:"pattern"`
	Temperature   *bool  `toml:"temperature,omitempty"`
	ReasoningKind string `toml:"reasoning_kind,omitempty"` // "none", "effort", "budget", "boolean"
	SystemRole    string `toml:"system_role,omitempty"`    // "system", "user", "developer"
}

type State struct {
	Provider               *string `toml:"provider,omitempty"`
	Model                  *string `toml:"model,omitempty"`
	ReasoningEffort        *string `toml:"reasoning_effort,omitempty"`
	FastModel              *string `toml:"fast_model,omitempty"`
	FastReasoningEffort    *string `toml:"fast_reasoning_effort,omitempty"`
	SummaryModel           *string `toml:"summary_model,omitempty"`
	SummaryReasoningEffort *string `toml:"summary_reasoning_effort,omitempty"`
	ActivePreset           *string `toml:"active_preset,omitempty"`
}

type RuntimeStateUpdate struct {
	Config              *Config
	PersistConfig       bool
	ActivePreset        string
	PersistActivePreset bool
	ReasoningPreset     string
	ReasoningEffort     string
	PersistReasoning    bool
}

func DefaultConfigPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "config.toml"), nil
}

func DefaultStatePath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "state.toml"), nil
}

func Load() (*Config, error) {
	cfg, err := LoadStable()
	if err != nil {
		return nil, err
	}
	state, err := LoadState()
	if err != nil {
		return nil, err
	}
	applyState(cfg, state)
	applyEnvOverrides(cfg)
	normalizeConfig(cfg)
	return cfg, nil
}

func LoadStable() (*Config, error) {
	cfg := defaultConfig()

	path, err := DefaultConfigPath()
	if err != nil {
		return nil, err
	}
	if data, err := os.ReadFile(path); err != nil {
		if !os.IsNotExist(err) {
			return nil, err
		}
	} else if err := toml.Unmarshal(data, cfg); err != nil {
		return nil, fmt.Errorf("failed to parse config: %w", err)
	}
	normalizeConfig(cfg)
	return cfg, nil
}

func applyEnvOverrides(cfg *Config) {
	providerOverride := os.Getenv("ION_PROVIDER")
	modelOverride := os.Getenv("ION_MODEL")

	if strings.TrimSpace(providerOverride) != "" {
		provider := normalizeProviderID(providerOverride)
		if provider != normalizeProviderID(cfg.Provider) &&
			strings.TrimSpace(modelOverride) == "" {
			cfg.Model = ""
		}
		if provider != normalizeProviderID(cfg.Provider) {
			clearProviderScopedPresets(cfg)
		}
		cfg.Provider = provider
	}

	if override := modelOverride; override != "" {
		if provider, model, ok := splitProviderModel(override); ok {
			if strings.TrimSpace(providerOverride) == "" {
				cfg.Provider = provider
			}
			cfg.Model = model
		} else {
			cfg.Model = override
		}
	}
	if override := os.Getenv("ION_REASONING_EFFORT"); override != "" {
		cfg.ReasoningEffort = override
	}
}

func normalizeConfig(cfg *Config) {
	cfg.Provider = normalizeProviderID(cfg.Provider)
	cfg.Model = strings.TrimSpace(cfg.Model)
	cfg.ReasoningEffort = normalizeReasoningEffort(cfg.ReasoningEffort)
	cfg.FastModel = strings.TrimSpace(cfg.FastModel)
	cfg.FastReasoningEffort = normalizeOptionalReasoningEffort(cfg.FastReasoningEffort)
	cfg.SummaryModel = strings.TrimSpace(cfg.SummaryModel)
	cfg.SummaryReasoningEffort = normalizeOptionalReasoningEffort(cfg.SummaryReasoningEffort)
	cfg.Endpoint = strings.TrimSpace(cfg.Endpoint)
	cfg.AuthEnvVar = strings.TrimSpace(cfg.AuthEnvVar)
	cfg.ExtraHeaders = normalizeStringMap(cfg.ExtraHeaders)
	cfg.TelemetryOTLPEndpoint = strings.TrimSpace(cfg.TelemetryOTLPEndpoint)
	cfg.TelemetryOTLPHeaders = normalizeStringMap(cfg.TelemetryOTLPHeaders)
	cfg.SubagentsPath = expandUserPath(strings.TrimSpace(cfg.SubagentsPath))
	cfg.ToolVerbosity = normalizeVerbosity(cfg.ToolVerbosity)
	cfg.ReadOutput = normalizeReadOutput(cfg.ReadOutput)
	cfg.WriteOutput = normalizeWriteOutput(cfg.WriteOutput)
	cfg.BashOutput = normalizeBashOutput(cfg.BashOutput)
	cfg.ThinkingVerbosity = normalizeVerbosity(cfg.ThinkingVerbosity)
	cfg.BusyInput = normalizeBusyInput(cfg.BusyInput)
	cfg.SkillTools = normalizeSkillTools(cfg.SkillTools)
	cfg.SubagentTools = normalizeSubagentTools(cfg.SubagentTools)
	cfg.ToolMode = normalizeToolMode(cfg.ToolMode)
	cfg.ToolEnv = normalizeToolEnv(cfg.ToolEnv)
	if cfg.ContextLimit < 0 {
		cfg.ContextLimit = 0
	}
	if cfg.MaxSessionCost < 0 {
		cfg.MaxSessionCost = 0
	}
	if cfg.MaxTurnCost < 0 {
		cfg.MaxTurnCost = 0
	}
	if cfg.SessionRetentionDays <= 0 {
		cfg.SessionRetentionDays = DefaultSessionRetentionDays
	}
}

func NormalizeVerbosity(value string) string {
	return normalizeVerbosity(value)
}

func LoadState() (*State, error) {
	state := &State{}
	path, err := DefaultStatePath()
	if err != nil {
		return nil, err
	}
	if data, err := os.ReadFile(path); err != nil {
		if !os.IsNotExist(err) {
			return nil, err
		}
	} else if err := toml.Unmarshal(data, state); err != nil {
		return nil, fmt.Errorf("failed to parse state: %w", err)
	}
	return state, nil
}

func SaveState(cfg *Config) error {
	current, err := LoadState()
	if err != nil {
		return err
	}
	state := stateFromConfig(cfg)
	state.ActivePreset = current.ActivePreset
	return saveState(state)
}

func SaveReasoningState(preset, effort string) error {
	state, err := LoadState()
	if err != nil {
		return err
	}
	normalized := normalizeOptionalReasoningEffort(effort)
	switch normalizeActivePreset(preset) {
	case "fast":
		state.FastReasoningEffort = optionalString(normalized)
	default:
		state.ReasoningEffort = optionalString(normalized)
	}
	return saveState(state)
}

func SaveActivePreset(preset string) error {
	state, err := LoadState()
	if err != nil {
		return err
	}
	normalized := normalizeActivePreset(preset)
	if normalized == "" {
		state.ActivePreset = nil
	} else {
		state.ActivePreset = &normalized
	}
	return saveState(state)
}

func SaveRuntimeState(update RuntimeStateUpdate) error {
	state, err := LoadState()
	if err != nil {
		return err
	}
	if update.PersistConfig {
		active := state.ActivePreset
		state = stateFromConfig(update.Config)
		state.ActivePreset = active
	}
	if update.PersistReasoning {
		normalized := normalizeOptionalReasoningEffort(update.ReasoningEffort)
		switch normalizeActivePreset(update.ReasoningPreset) {
		case "fast":
			state.FastReasoningEffort = optionalString(normalized)
		default:
			state.ReasoningEffort = optionalString(normalized)
		}
	}
	if update.PersistActivePreset {
		normalized := normalizeActivePreset(update.ActivePreset)
		if normalized == "" {
			state.ActivePreset = nil
		} else {
			state.ActivePreset = &normalized
		}
	}
	return saveState(state)
}

func saveState(state *State) error {
	path, err := DefaultStatePath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	data, err := toml.Marshal(state)
	if err != nil {
		return err
	}
	return writeFileAtomic(path, data, 0o644)
}

func Save(cfg *Config) error {
	path, err := DefaultConfigPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}

	out := *cfg
	out.Provider = normalizeProviderID(out.Provider)
	out.Model = strings.TrimSpace(out.Model)
	out.ReasoningEffort = normalizeReasoningEffort(out.ReasoningEffort)
	if out.ReasoningEffort == DefaultReasoningEffort {
		out.ReasoningEffort = ""
	}
	out.FastModel = strings.TrimSpace(out.FastModel)
	out.FastReasoningEffort = normalizeOptionalReasoningEffort(out.FastReasoningEffort)
	out.SummaryModel = strings.TrimSpace(out.SummaryModel)
	out.SummaryReasoningEffort = normalizeOptionalReasoningEffort(out.SummaryReasoningEffort)
	out.Endpoint = strings.TrimSpace(out.Endpoint)
	out.AuthEnvVar = strings.TrimSpace(out.AuthEnvVar)
	out.ExtraHeaders = normalizeStringMap(out.ExtraHeaders)
	out.TelemetryOTLPEndpoint = strings.TrimSpace(out.TelemetryOTLPEndpoint)
	out.TelemetryOTLPHeaders = normalizeStringMap(out.TelemetryOTLPHeaders)
	out.SubagentsPath = expandUserPath(strings.TrimSpace(out.SubagentsPath))
	if out.ContextLimit < 0 {
		out.ContextLimit = 0
	}
	if out.MaxSessionCost < 0 {
		out.MaxSessionCost = 0
	}
	if out.MaxTurnCost < 0 {
		out.MaxTurnCost = 0
	}
	if out.SessionRetentionDays <= 0 {
		out.SessionRetentionDays = DefaultSessionRetentionDays
	}
	out.ToolVerbosity = normalizeVerbosity(out.ToolVerbosity)
	out.ReadOutput = normalizeReadOutput(out.ReadOutput)
	out.WriteOutput = normalizeWriteOutput(out.WriteOutput)
	out.BashOutput = normalizeBashOutput(out.BashOutput)
	out.ThinkingVerbosity = normalizeVerbosity(out.ThinkingVerbosity)
	out.BusyInput = normalizeBusyInput(out.BusyInput)
	out.SkillTools = normalizeSkillTools(out.SkillTools)
	if out.SkillTools == "off" {
		out.SkillTools = ""
	}
	out.SubagentTools = normalizeSubagentTools(out.SubagentTools)
	if out.SubagentTools == "off" {
		out.SubagentTools = ""
	}
	out.ToolMode = normalizeToolMode(out.ToolMode)
	if out.ToolMode == "coding" {
		out.ToolMode = ""
	}
	out.ToolEnv = normalizeToolEnv(out.ToolEnv)
	if out.ToolEnv == "inherit" {
		out.ToolEnv = ""
	}

	data, err := toml.Marshal(&out)
	if err != nil {
		return err
	}
	return writeFileAtomic(path, data, 0o644)
}

func writeFileAtomic(path string, data []byte, perm os.FileMode) error {
	dir := filepath.Dir(path)
	tmp, err := os.CreateTemp(dir, "."+filepath.Base(path)+".tmp-*")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	defer func() {
		_ = os.Remove(tmpName)
	}()

	if _, err := tmp.Write(data); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Chmod(perm); err != nil {
		_ = tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmpName, path)
}

func applyState(cfg *Config, state *State) {
	if cfg == nil || state == nil {
		return
	}
	if state.Provider != nil {
		provider := normalizeProviderID(*state.Provider)
		if provider != normalizeProviderID(cfg.Provider) {
			clearProviderScopedPresets(cfg)
		}
		cfg.Provider = provider
	}
	if state.Model != nil {
		cfg.Model = strings.TrimSpace(*state.Model)
	}
	if state.ReasoningEffort != nil {
		cfg.ReasoningEffort = normalizeReasoningEffort(*state.ReasoningEffort)
	}
	if state.FastModel != nil {
		cfg.FastModel = strings.TrimSpace(*state.FastModel)
	}
	if state.FastReasoningEffort != nil {
		cfg.FastReasoningEffort = normalizeOptionalReasoningEffort(*state.FastReasoningEffort)
	}
	if state.SummaryModel != nil {
		cfg.SummaryModel = strings.TrimSpace(*state.SummaryModel)
	}
	if state.SummaryReasoningEffort != nil {
		cfg.SummaryReasoningEffort = normalizeOptionalReasoningEffort(*state.SummaryReasoningEffort)
	}
}

func clearProviderScopedPresets(cfg *Config) {
	if cfg == nil {
		return
	}
	cfg.FastModel = ""
	cfg.FastReasoningEffort = ""
	cfg.SummaryModel = ""
	cfg.SummaryReasoningEffort = ""
}

func stateFromConfig(cfg *Config) *State {
	if cfg == nil {
		return &State{}
	}
	provider := normalizeProviderID(cfg.Provider)
	model := strings.TrimSpace(cfg.Model)
	reasoning := normalizeOptionalReasoningEffort(cfg.ReasoningEffort)
	fastModel := strings.TrimSpace(cfg.FastModel)
	fastReasoning := normalizeOptionalReasoningEffort(cfg.FastReasoningEffort)
	summaryModel := strings.TrimSpace(cfg.SummaryModel)
	summaryReasoning := normalizeOptionalReasoningEffort(cfg.SummaryReasoningEffort)
	modelPtr := optionalString(model)
	if modelPtr == nil && provider != "" {
		modelPtr = &model
	}
	return &State{
		Provider:               optionalString(provider),
		Model:                  modelPtr,
		ReasoningEffort:        optionalString(reasoning),
		FastModel:              optionalString(fastModel),
		FastReasoningEffort:    optionalString(fastReasoning),
		SummaryModel:           optionalString(summaryModel),
		SummaryReasoningEffort: optionalString(summaryReasoning),
	}
}

func optionalString(value string) *string {
	if value == "" {
		return nil
	}
	return &value
}

func NormalizeActivePreset(value string) string {
	return normalizeActivePreset(value)
}

func NormalizeReasoningEffort(value string) string {
	return normalizeReasoningEffort(value)
}

func normalizeActivePreset(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "primary":
		return "primary"
	case "fast":
		return "fast"
	default:
		return ""
	}
}

func splitProviderModel(value string) (string, string, bool) {
	left, right, ok := strings.Cut(value, " ")
	if !ok {
		return "", "", false
	}

	provider := normalizeProviderID(left)
	model := strings.TrimSpace(right)
	if provider == "" || model == "" {
		return "", "", false
	}

	return provider, model, true
}

func normalizeProviderID(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "local-api", "custom-api":
		return "openai-compatible"
	default:
		return strings.ToLower(strings.TrimSpace(value))
	}
}

func DefaultDataDir() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "data"), nil
}

func DefaultModelCacheTTLSeconds() int {
	return defaultModelCacheTTLSeconds
}

func DefaultSubagentsDir() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "agents"), nil
}

func DefaultSkillsDir() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "skills"), nil
}

func defaultConfig() *Config {
	return &Config{
		SessionRetentionDays: DefaultSessionRetentionDays,
	}
}

func (c *Config) RetryUntilCancelledEnabled() bool {
	return c == nil || c.RetryUntilCancelled == nil || *c.RetryUntilCancelled
}

func (c *Config) BusyInputMode() string {
	if c != nil && normalizeBusyInput(c.BusyInput) == "queue" {
		return "queue"
	}
	return "steer"
}

func (c *Config) SkillToolMode() string {
	if c == nil {
		return "off"
	}
	return normalizeSkillTools(c.SkillTools)
}

func (c *Config) SubagentToolMode() string {
	if c == nil {
		return "off"
	}
	return normalizeSubagentTools(c.SubagentTools)
}

func (c *Config) ActiveToolMode() string {
	if c == nil {
		return "coding"
	}
	return normalizeToolMode(c.ToolMode)
}

func (c *Config) ToolEnvMode() string {
	if c == nil {
		return "inherit"
	}
	return normalizeToolEnv(c.ToolEnv)
}

func normalizeReasoningEffort(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", DefaultReasoningEffort:
		return DefaultReasoningEffort
	case "off", "none", "disabled":
		return "off"
	case "minimal", "min":
		return "minimal"
	case "low":
		return "low"
	case "medium", "med":
		return "medium"
	case "high":
		return "high"
	case "xhigh", "extra-high", "extra_high", "extra high":
		return "xhigh"
	case "max", "maximum":
		return "max"
	default:
		return DefaultReasoningEffort
	}
}

func normalizeOptionalReasoningEffort(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", DefaultReasoningEffort:
		return ""
	case "off", "none", "disabled":
		return "off"
	case "minimal", "min":
		return "minimal"
	case "low":
		return "low"
	case "medium", "med":
		return "medium"
	case "high":
		return "high"
	case "xhigh", "extra-high", "extra_high", "extra high":
		return "xhigh"
	case "max", "maximum":
		return "max"
	default:
		return ""
	}
}

func normalizeVerbosity(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "full":
		return "full"
	case "collapsed":
		return "collapsed"
	case "hidden":
		return "hidden"
	default:
		return ""
	}
}

func NormalizeReadOutput(value string) string {
	return normalizeReadOutput(value)
}

func normalizeReadOutput(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "show", "full":
		return "full"
	case "single", "summary", "line", "combined", "grouped", "collapsed":
		return "summary"
	case "hidden", "hide", "none":
		return "hidden"
	default:
		return ""
	}
}

func NormalizeWriteOutput(value string) string {
	return normalizeWriteOutput(value)
}

func normalizeWriteOutput(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "show", "diff", "full":
		return "diff"
	case "single", "summary", "call", "collapsed":
		return "summary"
	case "hidden", "hide", "none":
		return "hidden"
	default:
		return ""
	}
}

func NormalizeBashOutput(value string) string {
	return normalizeBashOutput(value)
}

func normalizeBashOutput(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "show", "full", "verbose":
		return "full"
	case "truncated", "summary", "collapsed":
		return "summary"
	case "hidden", "hide", "none", "command", "call":
		return "hidden"
	default:
		return ""
	}
}

func NormalizeBusyInput(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "queue", "queued", "followup", "follow-up", "follow_up":
		return "queue"
	case "steer", "steering":
		return "steer"
	default:
		return ""
	}
}

func normalizeBusyInput(value string) string {
	if NormalizeBusyInput(value) == "queue" {
		return "queue"
	}
	return ""
}

func normalizeSkillTools(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "read", "readonly", "read-only", "read_only":
		return "read"
	case "manage", "write", "full":
		return "manage"
	default:
		return "off"
	}
}

func normalizeSubagentTools(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "on", "true", "enabled", "enable", "subagent", "subagents", "delegate":
		return "on"
	default:
		return "off"
	}
}

func NormalizeToolMode(value string) string {
	return normalizeToolMode(value)
}

func normalizeToolMode(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "read", "readonly", "read-only", "read_only":
		return "read"
	case "all", "full":
		return "all"
	default:
		return "coding"
	}
}

func normalizeToolEnv(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "inherit_without_provider_keys":
		return "inherit_without_provider_keys"
	default:
		return "inherit"
	}
}

func expandUserPath(path string) string {
	if path == "~" {
		if home, err := os.UserHomeDir(); err == nil {
			return home
		}
		return path
	}
	if strings.HasPrefix(path, "~/") {
		if home, err := os.UserHomeDir(); err == nil {
			return filepath.Join(home, strings.TrimPrefix(path, "~/"))
		}
	}
	return path
}

func normalizeStringMap(values map[string]string) map[string]string {
	if len(values) == 0 {
		return nil
	}
	normalized := make(map[string]string, len(values))
	for key, value := range values {
		key = strings.TrimSpace(key)
		value = strings.TrimSpace(value)
		if key == "" || value == "" {
			continue
		}
		normalized[key] = value
	}
	if len(normalized) == 0 {
		return nil
	}
	return normalized
}
