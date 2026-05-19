package app

import (
	"fmt"
	"net/url"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/credentials"
	"github.com/nijaru/ion/internal/providers"
)

var (
	loadStableConfig = config.LoadStable
	loadConfigFile   = config.Load
	saveConfigFile   = config.Save
	saveProviderKey  = credentials.SaveAPIKey
)

func (m Model) openAPIKeyPrompt(
	cfg *config.Config,
	provider string,
	preset modelPreset,
) (Model, tea.Cmd) {
	def, ok := providers.Lookup(provider)
	if !ok {
		return m, cmdError(fmt.Sprintf("unsupported provider %q", strings.TrimSpace(provider)))
	}
	if !providerSupportsAPIKeyPrompt(def) {
		return m, cmdError(fmt.Sprintf("%s does not use API keys", def.DisplayName))
	}
	if cfg == nil {
		cfg = &config.Config{}
	}
	cfgCopy := *cfg
	cfgCopy.Provider = def.ID
	m.Picker.Overlay = nil
	m.Picker.Setup = &setupPromptState{
		kind:         setupPromptAPIKey,
		provider:     def.ID,
		providerName: def.DisplayName,
		preset:       preset,
		cfg:          cfgCopy,
	}
	return m, nil
}

func providerSupportsAPIKeyPrompt(def providers.Definition) bool {
	switch def.AuthKind {
	case providers.AuthAPIKey, providers.AuthToken, providers.AuthOptional:
		return true
	default:
		return false
	}
}

func (m Model) openEndpointPrompt(cfg *config.Config, preset modelPreset) (Model, tea.Cmd) {
	if cfg == nil {
		cfg = &config.Config{}
	}
	cfgCopy := *cfg
	cfgCopy.Provider = providers.OpenAICompatibleID
	m.Picker.Overlay = nil
	m.Picker.Setup = &setupPromptState{
		kind:         setupPromptEndpoint,
		provider:     providers.OpenAICompatibleID,
		providerName: providers.DisplayName(providers.OpenAICompatibleID),
		value:        strings.TrimSpace(cfgCopy.Endpoint),
		preset:       preset,
		cfg:          cfgCopy,
	}
	return m, nil
}

func (m Model) handleSetupPromptKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.Picker.Setup == nil {
		return m, nil
	}
	if m.Picker.Setup.saving {
		return m, nil
	}
	switch msg.String() {
	case "esc", "ctrl+c", "ctrl+d":
		m.Picker.Setup = nil
		return m, nil
	case "backspace":
		if m.Picker.Setup.value != "" {
			m.Picker.Setup.value = trimLastRune(m.Picker.Setup.value)
		}
		return m, nil
	case "enter":
		return m.commitSetupPrompt()
	default:
		if text, ok := keyTextInput(msg); ok {
			m.Picker.Setup.value += text
			m.Picker.Setup.err = ""
		}
		return m, nil
	}
}

func (m Model) handleSetupPromptPaste(msg tea.PasteMsg) (Model, tea.Cmd) {
	if m.Picker.Setup == nil {
		return m, nil
	}
	if m.Picker.Setup.saving {
		return m, nil
	}
	content := inlinePasteText(msg.Content)
	if content == "" {
		return m, nil
	}
	m.Picker.Setup.value += content
	m.Picker.Setup.err = ""
	return m, nil
}

func (m Model) commitSetupPrompt() (Model, tea.Cmd) {
	prompt := m.Picker.Setup
	if prompt == nil {
		return m, nil
	}
	if m.localCommandBusy() {
		prompt.err = m.localCommandBusyMessage("saving provider setup")
		return m, cmdError(prompt.err)
	}
	switch prompt.kind {
	case setupPromptAPIKey:
		key := strings.TrimSpace(prompt.value)
		if key == "" {
			m.Picker.Setup.err = "API key cannot be empty"
			return m, nil
		}
		m.Picker.SetupSaveRequest++
		requestID := m.Picker.SetupSaveRequest
		prompt.saving = true
		prompt.request = requestID
		prompt.err = ""
		m.Progress.Status = "Saving provider setup..."
		cfg := prompt.cfg
		provider := prompt.provider
		preset := prompt.preset
		return m, func() tea.Msg {
			err := saveProviderKey(provider, key)
			return setupPromptSavedMsg{
				requestID: requestID,
				cfg:       cfg,
				preset:    preset,
				err:       err,
			}
		}
	case setupPromptEndpoint:
		endpoint, err := normalizeOpenAICompatibleEndpoint(prompt.value)
		if err != nil {
			m.Picker.Setup.err = err.Error()
			return m, nil
		}
		m.Picker.SetupSaveRequest++
		requestID := m.Picker.SetupSaveRequest
		prompt.saving = true
		prompt.request = requestID
		prompt.err = ""
		m.Progress.Status = "Saving provider setup..."
		cfg := prompt.cfg
		cfg.Endpoint = endpoint
		preset := prompt.preset
		return m, func() tea.Msg {
			stable, err := loadStableConfig()
			if err != nil {
				return setupPromptSavedMsg{requestID: requestID, err: err}
			}
			stable.Endpoint = endpoint
			if err := saveConfigFile(stable); err != nil {
				return setupPromptSavedMsg{requestID: requestID, err: err}
			}
			return setupPromptSavedMsg{
				requestID: requestID,
				cfg:       cfg,
				preset:    preset,
			}
		}
	default:
		m.Picker.Setup = nil
		return m, nil
	}
}

func (m Model) handleSetupPromptSaved(msg setupPromptSavedMsg) (Model, tea.Cmd) {
	if msg.requestID == 0 || msg.requestID != m.Picker.SetupSaveRequest {
		return m, nil
	}
	m.Picker.SetupSaveRequest = 0
	if m.Picker.Setup == nil || m.Picker.Setup.request != msg.requestID {
		return m, nil
	}
	if msg.err != nil {
		m.Picker.Setup.saving = false
		m.Picker.Setup.request = 0
		m.Picker.Setup.err = msg.err.Error()
		if isLocalBusyStatus(m.Progress.Status) {
			m.Progress.Status = ""
		}
		return m, nil
	}
	m.Picker.Setup = nil
	if isLocalBusyStatus(m.Progress.Status) {
		m.Progress.Status = ""
	}
	cfg := msg.cfg
	return m.openModelPickerForPreset(&cfg, msg.preset)
}

func trimLastRune(value string) string {
	if value == "" {
		return ""
	}
	_, size := utf8.DecodeLastRuneInString(value)
	return value[:len(value)-size]
}

func normalizeOpenAICompatibleEndpoint(raw string) (string, error) {
	value := strings.TrimSpace(raw)
	if value == "" {
		return "", fmt.Errorf("endpoint cannot be empty")
	}
	if !strings.Contains(value, "://") {
		value = "http://" + value
	}
	parsed, err := url.Parse(value)
	if err != nil || parsed.Host == "" {
		return "", fmt.Errorf("invalid endpoint")
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return "", fmt.Errorf("endpoint must use http or https")
	}
	if parsed.Path == "" || parsed.Path == "/" {
		parsed.Path = "/v1"
	}
	parsed.RawQuery = ""
	parsed.Fragment = ""
	return strings.TrimRight(parsed.String(), "/"), nil
}
