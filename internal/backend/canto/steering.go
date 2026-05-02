package canto

import (
	"context"
	"fmt"
	"strings"
	"sync"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/prompt"
	csession "github.com/nijaru/canto/session"
	ionsession "github.com/nijaru/ion/internal/session"
)

const steeringKind = "ion_steering"

type steeringMutator struct {
	mu      sync.Mutex
	pending map[string][]string
}

type steeringEvent struct {
	Kind           string `json:"kind"`
	Status         string `json:"status"`
	Input          string `json:"input,omitzero"`
	PendingEventID string `json:"pending_event_id,omitzero"`
}

func newSteeringMutator() *steeringMutator {
	return &steeringMutator{pending: make(map[string][]string)}
}

func (m *steeringMutator) Submit(
	_ context.Context,
	sessionID string,
	text string,
) (ionsession.SteeringResult, error) {
	text = strings.TrimSpace(text)
	if text == "" {
		return ionsession.SteeringResult{}, fmt.Errorf("steering text is empty")
	}

	m.mu.Lock()
	m.pending[sessionID] = append(m.pending[sessionID], text)
	m.mu.Unlock()

	return ionsession.SteeringResult{
		Outcome: ionsession.SteeringAccepted,
		Notice:  "Steering will be applied at the next provider boundary.",
	}, nil
}

func (m *steeringMutator) Mutate(
	ctx context.Context,
	_ llm.Provider,
	_ string,
	sess *csession.Session,
) error {
	items := m.pendingFor(sess.ID())
	if len(items) == 0 {
		return nil
	}

	for _, item := range items {
		pending := csession.NewEvent(sess.ID(), csession.ExternalInput, steeringEvent{
			Kind:   steeringKind,
			Status: "pending",
			Input:  item,
		})
		if err := sess.Append(ctx, pending); err != nil {
			return err
		}

		contextEvent := csession.NewContext(sess.ID(), csession.ContextEntry{
			Kind:      csession.ContextKindGeneric,
			Placement: csession.ContextPlacementHistory,
			Content:   steeringContext(item),
		})
		contextEvent.Metadata = map[string]any{
			"kind":             steeringKind,
			"pending_event_id": pending.ID.String(),
		}
		if err := sess.Append(ctx, contextEvent); err != nil {
			return err
		}

		if err := sess.Append(ctx, csession.NewEvent(sess.ID(), csession.ExternalInput, steeringEvent{
			Kind:           steeringKind,
			Status:         "consumed",
			PendingEventID: pending.ID.String(),
		})); err != nil {
			return err
		}
	}

	m.drop(sess.ID(), len(items))
	return nil
}

func (m *steeringMutator) Effects() prompt.SideEffects {
	return prompt.SideEffects{Session: true}
}

func (m *steeringMutator) pendingFor(sessionID string) []string {
	m.mu.Lock()
	defer m.mu.Unlock()
	items := m.pending[sessionID]
	if len(items) == 0 {
		return nil
	}
	return append([]string(nil), items...)
}

func (m *steeringMutator) drop(sessionID string, n int) {
	m.mu.Lock()
	defer m.mu.Unlock()
	items := m.pending[sessionID]
	if n >= len(items) {
		delete(m.pending, sessionID)
		return
	}
	m.pending[sessionID] = append([]string(nil), items[n:]...)
}

func steeringContext(input string) string {
	return strings.TrimSpace(`<user_steering>
The user sent this while the current turn was running. Treat it as guidance for the next step if relevant, without repeating completed work.

` + strings.TrimSpace(input) + `
</user_steering>`)
}
