package config

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os/exec"
	"runtime"
	"strings"
	"sync"
	"time"
)

// OpenAI Codex OAuth parameters match the ChatGPT subscription client.
const (
	OpenAIOAuthIssuer        = "https://auth.openai.com"
	OpenAICodexOAuthClientID = "app_EMoamEEZ73f0CkXaXp7hrann"
	OpenAICodexOAuthScope    = "openid profile email offline_access"
	DefaultOAuthPort         = 1455
	FallbackOAuthPort        = 1457
)

// OAuthProviderConfig defines the OAuth endpoints and client configuration for a provider.
type OAuthProviderConfig struct {
	Issuer               string
	AuthURL              string
	TokenURL             string
	ClientID             string
	Scopes               []string
	DefaultPort          int
	FixedRedirectPort    bool
	AccountIDFromJWT     bool
	AuthorizeQueryValues map[string]string
}

// KnownOAuthProviders returns the standard OAuth configurations for supported providers.
func KnownOAuthProviders() map[string]OAuthProviderConfig {
	return map[string]OAuthProviderConfig{
		"openai-codex": {
			Issuer:            OpenAIOAuthIssuer,
			AuthURL:           "https://auth.openai.com/oauth/authorize",
			TokenURL:          "https://auth.openai.com/oauth/token",
			ClientID:          OpenAICodexOAuthClientID,
			Scopes:            strings.Fields(OpenAICodexOAuthScope),
			DefaultPort:       DefaultOAuthPort,
			FixedRedirectPort: true,
			AccountIDFromJWT:  true,
			AuthorizeQueryValues: map[string]string{
				"id_token_add_organizations": "true",
				"codex_cli_simplified_flow":  "true",
				"originator":                 "ion",
			},
		},
	}
}

// OAuthTokens holds the exchanged OAuth 2.0 credentials.
type OAuthTokens struct {
	AccessToken  string `json:"access_token"`
	RefreshToken string `json:"refresh_token,omitempty"`
	TokenType    string `json:"token_type,omitempty"`
	ExpiresIn    int64  `json:"expires_in,omitempty"`
	ExpiresAt    int64  `json:"expires_at,omitempty"`
	AccountID    string `json:"account_id,omitempty"`
}

// GeneratePKCE generates a random code verifier and its S256 code challenge.
func GeneratePKCE() (verifier, challenge string, err error) {
	bytes := make([]byte, 32)
	if _, err := rand.Read(bytes); err != nil {
		return "", "", fmt.Errorf("generate random verifier: %w", err)
	}
	verifier = base64.RawURLEncoding.EncodeToString(bytes)
	h := sha256.Sum256([]byte(verifier))
	challenge = base64.RawURLEncoding.EncodeToString(h[:])
	return verifier, challenge, nil
}

// GenerateRandomState generates a secure random state string for OAuth CSRF protection.
func GenerateRandomState() (string, error) {
	bytes := make([]byte, 16)
	if _, err := rand.Read(bytes); err != nil {
		return "", fmt.Errorf("generate random state: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(bytes), nil
}

// StartOAuthFlowOptions configures the interactive browser login.
type StartOAuthFlowOptions struct {
	Provider         string
	OpenBrowser      bool
	HTTPClient       *http.Client
	URLCallback      func(authURL string)
	AuthURLOverride  string
	TokenURLOverride string
	PortOverride     int
}

// StartPKCEOAuthFlow orchestrates the local loopback server, PKCE generation, and token exchange.
func StartPKCEOAuthFlow(ctx context.Context, opts StartOAuthFlowOptions) (*OAuthTokens, error) {
	providers := KnownOAuthProviders()
	cfg, ok := providers[strings.ToLower(strings.TrimSpace(opts.Provider))]
	if !ok {
		return nil, fmt.Errorf("provider %q does not support OAuth login", opts.Provider)
	}
	if opts.AuthURLOverride != "" {
		cfg.AuthURL = opts.AuthURLOverride
	}
	if opts.TokenURLOverride != "" {
		cfg.TokenURL = opts.TokenURLOverride
	}
	if opts.PortOverride > 0 {
		cfg.DefaultPort = opts.PortOverride
		cfg.FixedRedirectPort = false
	}

	verifier, challenge, err := GeneratePKCE()
	if err != nil {
		return nil, err
	}

	state, err := GenerateRandomState()
	if err != nil {
		return nil, err
	}

	ports := []int{cfg.DefaultPort, FallbackOAuthPort, 0}
	if cfg.FixedRedirectPort {
		ports = []int{cfg.DefaultPort}
	}
	listener, port, err := listenOnAvailablePort(ports)
	if err != nil {
		return nil, fmt.Errorf("start local OAuth callback listener: %w", err)
	}
	defer listener.Close()

	redirectURI := fmt.Sprintf("http://localhost:%d/auth/callback", port)

	authValues := url.Values{}
	authValues.Set("response_type", "code")
	authValues.Set("client_id", cfg.ClientID)
	authValues.Set("redirect_uri", redirectURI)
	authValues.Set("scope", strings.Join(cfg.Scopes, " "))
	authValues.Set("code_challenge", challenge)
	authValues.Set("code_challenge_method", "S256")
	authValues.Set("state", state)
	for key, value := range cfg.AuthorizeQueryValues {
		authValues.Set(key, value)
	}

	authURL := fmt.Sprintf("%s?%s", cfg.AuthURL, authValues.Encode())

	type callbackResult struct {
		code string
		err  error
	}
	resultChan := make(chan callbackResult, 1)
	var once sync.Once

	mux := http.NewServeMux()
	mux.HandleFunc("/auth/callback", func(w http.ResponseWriter, r *http.Request) {
		q := r.URL.Query()
		callbackState := q.Get("state")
		if callbackState != state {
			http.Error(w, "Invalid OAuth state parameter", http.StatusBadRequest)
			once.Do(func() {
				resultChan <- callbackResult{err: errors.New("state mismatch")}
			})
			return
		}

		if errParam := q.Get("error"); errParam != "" {
			errDesc := q.Get("error_description")
			if errDesc == "" {
				errDesc = errParam
			}
			http.Error(w, "Authentication failed: "+errDesc, http.StatusBadRequest)
			once.Do(func() {
				resultChan <- callbackResult{err: fmt.Errorf("oauth error: %s", errDesc)}
			})
			return
		}

		code := q.Get("code")
		if code == "" {
			http.Error(w, "Missing authorization code", http.StatusBadRequest)
			once.Do(func() {
				resultChan <- callbackResult{err: errors.New("missing authorization code")}
			})
			return
		}

		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = fmt.Fprint(w, `<!DOCTYPE html>
<html>
<head><title>Ion Authentication</title><style>body{font-family:-apple-system,BlinkMacSystemFont,sans-serif;text-align:center;padding:50px;background:#111;color:#eee}h1{color:#4ade80}</style></head>
<body>
  <h1>Authentication Successful</h1>
  <p>You may now close this browser tab and return to Ion.</p>
</body>
</html>`)

		once.Do(func() {
			resultChan <- callbackResult{code: code}
		})
	})

	server := &http.Server{Handler: mux}
	go func() {
		_ = server.Serve(listener)
	}()
	defer func() {
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = server.Shutdown(shutdownCtx)
	}()

	if opts.URLCallback != nil {
		opts.URLCallback(authURL)
	}

	if opts.OpenBrowser {
		_ = openBrowserURL(authURL)
	}

	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case res := <-resultChan:
		if res.err != nil {
			return nil, res.err
		}

		client := opts.HTTPClient
		if client == nil {
			client = http.DefaultClient
		}

		tokens, err := exchangeCodeForTokens(ctx, client, cfg.TokenURL, cfg.ClientID, res.code, verifier, redirectURI)
		if err != nil {
			return nil, fmt.Errorf("exchange authorization code: %w", err)
		}
		if cfg.AccountIDFromJWT {
			tokens.AccountID, err = accountIDFromJWT(tokens.AccessToken)
			if err != nil {
				return nil, err
			}
		}
		if strings.TrimSpace(tokens.RefreshToken) == "" {
			return nil, errors.New("OAuth token response has no refresh token")
		}

		if err := SaveOAuthCredentials(opts.Provider, tokens); err != nil {
			return nil, fmt.Errorf("save OAuth credentials: %w", err)
		}
		return tokens, nil
	}
}

func accountIDFromJWT(token string) (string, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return "", errors.New("OpenAI Codex access token is not a JWT")
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return "", fmt.Errorf("decode OpenAI Codex access token: %w", err)
	}
	var claims struct {
		Auth struct {
			ChatGPTAccountID string `json:"chatgpt_account_id"`
		} `json:"https://api.openai.com/auth"`
	}
	if err := json.Unmarshal(payload, &claims); err != nil {
		return "", fmt.Errorf("decode OpenAI Codex access token claims: %w", err)
	}
	if strings.TrimSpace(claims.Auth.ChatGPTAccountID) == "" {
		return "", errors.New("OpenAI Codex access token has no ChatGPT account ID")
	}
	return strings.TrimSpace(claims.Auth.ChatGPTAccountID), nil
}

// RefreshOAuthCredentials refreshes a stored OAuth credential when the
// provider supports refresh tokens and persists the replacement atomically.
func RefreshOAuthCredentials(ctx context.Context, provider string) (*OAuthTokens, error) {
	provider = strings.ToLower(strings.TrimSpace(provider))
	providerConfig, ok := KnownOAuthProviders()[provider]
	if !ok {
		return nil, fmt.Errorf("provider %q does not support OAuth refresh", provider)
	}
	stored, ok := LookupOAuthTokens(provider)
	if !ok || strings.TrimSpace(stored.RefreshToken) == "" {
		return nil, fmt.Errorf("provider %q has no refresh token", provider)
	}
	tokens, err := refreshOAuthCredential(ctx, http.DefaultClient, providerConfig, stored)
	if err != nil {
		return nil, err
	}
	if err := SaveOAuthCredentials(provider, tokens); err != nil {
		return nil, fmt.Errorf("save refreshed OAuth credentials: %w", err)
	}
	return tokens, nil
}

func refreshOAuthCredential(
	ctx context.Context,
	client *http.Client,
	provider OAuthProviderConfig,
	stored CredentialProvider,
) (*OAuthTokens, error) {
	if client == nil {
		client = http.DefaultClient
	}
	data := url.Values{
		"grant_type":    {"refresh_token"},
		"refresh_token": {stored.RefreshToken},
		"client_id":     {provider.ClientID},
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, provider.TokenURL, strings.NewReader(data.Encode()))
	if err != nil {
		return nil, fmt.Errorf("create OAuth refresh request: %w", err)
	}
	request.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	response, err := client.Do(request)
	if err != nil {
		return nil, fmt.Errorf("refresh OAuth credentials: %w", err)
	}
	defer response.Body.Close()
	tokens, err := decodeOAuthTokenResponse(response)
	if err != nil {
		return nil, err
	}
	if tokens.RefreshToken == "" {
		tokens.RefreshToken = stored.RefreshToken
	}
	if provider.AccountIDFromJWT {
		accountID, accountErr := accountIDFromJWT(tokens.AccessToken)
		if accountErr != nil {
			return nil, accountErr
		}
		if stored.AccountID != "" && stored.AccountID != accountID {
			return nil, errors.New("OpenAI Codex refresh changed ChatGPT account")
		}
		tokens.AccountID = accountID
	} else if tokens.AccountID == "" {
		tokens.AccountID = stored.AccountID
	}
	return tokens, nil
}

func exchangeCodeForTokens(
	ctx context.Context,
	client *http.Client,
	tokenURL, clientID, code, verifier, redirectURI string,
) (*OAuthTokens, error) {
	data := url.Values{}
	data.Set("grant_type", "authorization_code")
	data.Set("client_id", clientID)
	data.Set("code", code)
	data.Set("code_verifier", verifier)
	data.Set("redirect_uri", redirectURI)

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, tokenURL, strings.NewReader(data.Encode()))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")

	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	return decodeOAuthTokenResponse(resp)
}

func decodeOAuthTokenResponse(resp *http.Response) (*OAuthTokens, error) {
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("token endpoint returned status %d: %s", resp.StatusCode, string(body))
	}
	var tokens OAuthTokens
	if err := json.Unmarshal(body, &tokens); err != nil {
		return nil, fmt.Errorf("decode token response: %w", err)
	}
	if strings.TrimSpace(tokens.AccessToken) == "" {
		return nil, errors.New("token response has no access token")
	}
	if tokens.ExpiresIn <= 0 {
		return nil, errors.New("token response has no positive expiry")
	}
	tokens.ExpiresAt = time.Now().Unix() + tokens.ExpiresIn
	return &tokens, nil
}

func listenOnAvailablePort(ports []int) (net.Listener, int, error) {
	for _, port := range ports {
		addr := fmt.Sprintf("127.0.0.1:%d", port)
		listener, err := net.Listen("tcp", addr)
		if err == nil {
			tcpAddr, ok := listener.Addr().(*net.TCPAddr)
			if ok {
				return listener, tcpAddr.Port, nil
			}
			return listener, port, nil
		}
	}
	return nil, 0, errors.New("no available port for OAuth callback server")
}

func openBrowserURL(targetURL string) error {
	switch runtime.GOOS {
	case "darwin":
		return exec.Command("open", targetURL).Start()
	case "linux":
		return exec.Command("xdg-open", targetURL).Start()
	case "windows":
		return exec.Command("rundll32", "url.dll,FileProtocolHandler", targetURL).Start()
	default:
		return errors.New("unsupported platform for openBrowserURL")
	}
}
