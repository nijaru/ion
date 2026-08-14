package app

import (
	"context"
	"fmt"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
)

type oauthLoginFinishedMsg struct {
	provider string
	tokens   *config.OAuthTokens
	err      error
}

func (m Model) startBrowserOAuthLogin(provider string) (Model, tea.Cmd) {
	provider = strings.ToLower(strings.TrimSpace(provider))
	known := config.KnownOAuthProviders()
	cfg, ok := known[provider]
	if !ok {
		return m, cmdError(fmt.Sprintf("provider %q does not support browser OAuth", provider))
	}

	startMsg := fmt.Sprintf("Starting browser login for %s (port %d)...", strings.ToUpper(provider), cfg.DefaultPort)
	return m, tea.Batch(
		m.terminalCommit().Entries(systemEntry(startMsg)),
		func() tea.Msg {
			ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
			defer cancel()

			tokens, err := config.StartPKCEOAuthFlow(ctx, config.StartOAuthFlowOptions{
				Provider:    provider,
				OpenBrowser: true,
			})
			return oauthLoginFinishedMsg{
				provider: provider,
				tokens:   tokens,
				err:      err,
			}
		},
	)
}

func (m Model) handleOAuthLoginFinished(msg oauthLoginFinishedMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		return m, m.terminalCommit().Entries(
			systemEntry(fmt.Sprintf("Authentication failed for %s: %v", strings.ToUpper(msg.provider), msg.err)),
		)
	}

	successMsg := fmt.Sprintf(
		"Authentication successful! %s credentials saved to ~/.ion/credentials.toml.",
		strings.ToUpper(msg.provider),
	)
	next, reloadCmd := m.reloadConfig()
	return next, tea.Batch(
		next.terminalCommit().Entries(systemEntry(successMsg)),
		reloadCmd,
	)
}
