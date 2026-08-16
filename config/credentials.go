package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

type CredentialProvider struct {
	APIKey       string `toml:"api_key,omitempty"`
	AccessToken  string `toml:"access_token,omitempty"`
	RefreshToken string `toml:"refresh_token,omitempty"`
	TokenType    string `toml:"token_type,omitempty"`
	ExpiresAt    int64  `toml:"expires_at,omitempty"`
	AccountID    string `toml:"account_id,omitempty"`
	AuthKind     string `toml:"auth_kind,omitempty"`
}

type CredentialsFile struct {
	Providers map[string]CredentialProvider `toml:"providers,omitempty"`
}

func CredentialPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "credentials.toml"), nil
}

func LookupAPIKey(provider string) (string, bool) {
	file, err := LoadCredentials()
	if err != nil {
		return "", false
	}
	credential, ok := file.Providers[normalizeCredentialProvider(provider)]
	if !ok {
		return "", false
	}
	// Check APIKey first, then AccessToken if OAuth is active
	key := strings.TrimSpace(credential.APIKey)
	if key != "" {
		return key, true
	}
	token := strings.TrimSpace(credential.AccessToken)
	return token, token != ""
}

// LookupOAuthTokens retrieves the stored OAuth credentials for a provider.
func LookupOAuthTokens(provider string) (CredentialProvider, bool) {
	file, err := LoadCredentials()
	if err != nil {
		return CredentialProvider{}, false
	}
	credential, ok := file.Providers[normalizeCredentialProvider(provider)]
	if !ok || strings.TrimSpace(credential.AccessToken) == "" {
		return CredentialProvider{}, false
	}
	return credential, true
}

// SaveOAuthCredentials saves the exchanged OAuth tokens.
func SaveOAuthCredentials(provider string, tokens *OAuthTokens) error {
	provider = normalizeCredentialProvider(provider)
	if provider == "" {
		return fmt.Errorf("provider is required")
	}
	if tokens == nil {
		return fmt.Errorf("tokens cannot be nil")
	}

	file, err := LoadCredentials()
	if err != nil {
		return err
	}
	if file.Providers == nil {
		file.Providers = map[string]CredentialProvider{}
	}

	file.Providers[provider] = CredentialProvider{
		AccessToken:  tokens.AccessToken,
		RefreshToken: tokens.RefreshToken,
		TokenType:    tokens.TokenType,
		ExpiresAt:    tokens.ExpiresAt,
		AccountID:    tokens.AccountID,
		AuthKind:     "oauth",
	}
	return SaveCredentials(file)
}

// HasAPIKey returns true if the provider has a configured API key.
func HasAPIKey(provider string) bool {
	_, ok := LookupAPIKey(provider)
	return ok
}

func SaveAPIKey(provider, key string) error {
	provider = normalizeCredentialProvider(provider)
	key = strings.TrimSpace(key)
	if provider == "" {
		return fmt.Errorf("provider is required")
	}

	file, err := LoadCredentials()
	if err != nil {
		return err
	}
	if file.Providers == nil {
		file.Providers = map[string]CredentialProvider{}
	}

	// Pi: empty key deletes the credential (logout).
	if key == "" {
		delete(file.Providers, provider)
		return SaveCredentials(file)
	}

	file.Providers[provider] = CredentialProvider{APIKey: key}
	return SaveCredentials(file)
}

func LoadCredentials() (*CredentialsFile, error) {
	path, err := CredentialPath()
	if err != nil {
		return nil, err
	}
	file := &CredentialsFile{Providers: map[string]CredentialProvider{}}
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return file, nil
		}
		return nil, err
	}
	if err := toml.Unmarshal(data, file); err != nil {
		return nil, fmt.Errorf("failed to parse credentials: %w", err)
	}
	if file.Providers == nil {
		file.Providers = map[string]CredentialProvider{}
	}
	return file, nil
}

func SaveCredentials(file *CredentialsFile) error {
	if file == nil {
		file = &CredentialsFile{}
	}
	if file.Providers == nil {
		file.Providers = map[string]CredentialProvider{}
	}
	path, err := CredentialPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	data, err := toml.Marshal(file)
	if err != nil {
		return err
	}
	return writeFileAtomic(path, data, 0o600)
}

func normalizeCredentialProvider(provider string) string {
	switch strings.ToLower(strings.TrimSpace(provider)) {
	case "local-api", "custom-api":
		return "openai-compatible"
	default:
		return strings.ToLower(strings.TrimSpace(provider))
	}
}
