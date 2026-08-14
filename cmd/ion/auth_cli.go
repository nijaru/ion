package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"strings"
	"time"

	"github.com/nijaru/ion/config"
)

func authCommandUsage() string {
	return `Usage: ion auth <subcommand> [flags]

Subcommands:
  login [provider]    Authenticate via browser OAuth 2.0 PKCE (default: openai)
  logout <provider>   Remove stored credentials for a provider
  status              Show active credential types for each provider
`
}

func runAuthCommand(ctx context.Context, args []string, stdout io.Writer) error {
	if len(args) == 0 {
		return errors.New(authCommandUsage())
	}
	switch args[0] {
	case "login":
		provider := "openai"
		if len(args) > 1 {
			provider = args[1]
		}
		return runAuthLogin(ctx, stdout, provider)
	case "logout":
		if len(args) < 2 {
			return fmt.Errorf("usage: ion auth logout <provider>")
		}
		return runAuthLogout(stdout, args[1])
	case "status":
		return runAuthStatus(stdout)
	default:
		return errors.New(authCommandUsage())
	}
}

func runAuthLogin(ctx context.Context, w io.Writer, provider string) error {
	provider = strings.ToLower(strings.TrimSpace(provider))
	if provider == "" {
		provider = "openai"
	}

	known := config.KnownOAuthProviders()
	if _, ok := known[provider]; !ok {
		return fmt.Errorf("provider %q does not support browser OAuth login. Use API key or /login in TUI", provider)
	}

	fmt.Fprintf(w, "Starting browser login for %s...\n", strings.ToUpper(provider))

	authCtx, cancel := context.WithTimeout(ctx, 3*time.Minute)
	defer cancel()

	tokens, err := config.StartPKCEOAuthFlow(authCtx, config.StartOAuthFlowOptions{
		Provider:    provider,
		OpenBrowser: true,
		URLCallback: func(authURL string) {
			fmt.Fprintf(w, "Opening your browser to authorize Ion:\n\n  %s\n\nWaiting for authorization...\n", authURL)
		},
	})
	if err != nil {
		return fmt.Errorf("authentication failed: %w", err)
	}

	fmt.Fprintf(w, "\nAuthentication successful! %s OAuth credentials saved to ~/.ion/credentials.toml.\n", strings.ToUpper(provider))
	if tokens.ExpiresIn > 0 {
		fmt.Fprintf(w, "Access token valid for %d minutes (automatic background refresh enabled).\n", tokens.ExpiresIn/60)
	}
	return nil
}

func runAuthLogout(w io.Writer, provider string) error {
	provider = strings.ToLower(strings.TrimSpace(provider))
	if err := config.SaveAPIKey(provider, ""); err != nil {
		return fmt.Errorf("failed to clear credentials: %w", err)
	}
	fmt.Fprintf(w, "Cleared credentials for %s.\n", strings.ToUpper(provider))
	return nil
}

func runAuthStatus(w io.Writer) error {
	creds, err := config.LoadCredentials()
	if err != nil {
		return fmt.Errorf("failed to load credentials: %w", err)
	}
	if len(creds.Providers) == 0 {
		fmt.Fprintln(w, "No credentials saved in ~/.ion/credentials.toml.")
		return nil
	}

	fmt.Fprintln(w, "Saved Provider Credentials:")
	for name, prov := range creds.Providers {
		kind := prov.AuthKind
		if kind == "" {
			kind = "api_key"
		}
		if kind == "oauth" {
			expiresStr := "not expired"
			if prov.ExpiresAt > 0 && time.Now().Unix() > prov.ExpiresAt {
				expiresStr = "expired (will refresh on next request)"
			}
			fmt.Fprintf(w, "  • %-12s OAuth (%s)\n", strings.ToUpper(name), expiresStr)
		} else {
			fmt.Fprintf(w, "  • %-12s API Key\n", strings.ToUpper(name))
		}
	}
	return nil
}
