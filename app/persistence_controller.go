package app

import (
	"context"
	"fmt"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/session"
)

type persistenceController struct {
	storage session.SessionHandle
}

func (m Model) persistenceController() persistenceController {
	return persistenceController{storage: m.Model.Storage}
}

func (c persistenceController) appendEntry(action string, entry session.StoreEvent) tea.Cmd {
	if c.storage == nil {
		return nil
	}
	return func() tea.Msg {
		if err := c.storage.Append(context.Background(), entry); err != nil {
			return localErrorMsg{err: fmt.Errorf("%s: %w", action, err)}
		}
		return nil
	}
}
