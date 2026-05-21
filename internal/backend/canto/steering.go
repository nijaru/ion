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
	pending map[string][]pendingSteering
}

type pendingSteering struct {
	turnID string
	text   string
}

type steeringEvent struct {
	Kind           string `json:"kind"`
	Status         string `json:"status"`
	Input          string `json:"input,omitzero"`
	PendingEventID string `json:"pending_event_id,omitzero"`
}

func newSteeringMutator() *steeringMutator {
	return &steeringMutator{pending: make(map[string][]pendingSteering)}
}

func (m *steeringMutator) Submit(
	_ context.Context,
	sessionID string,
	turnID string,
	text string,
) (ionsession.SteeringResult, error) {
	text = strings.TrimSpace(text)
	if text == "" {
		return ionsession.SteeringResult{}, fmt.Errorf("steering text is empty")
	}
	turnID = strings.TrimSpace(turnID)
	if turnID == "" {
		return ionsession.SteeringResult{}, fmt.Errorf("steering turn id is empty")
	}

	m.mu.Lock()
	m.pending[sessionID] = append(m.pending[sessionID], pendingSteering{
		turnID: turnID,
		text:   text,
	})
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
	sessionID := sess.ID()
	turnID := csession.TurnIDFromContext(ctx)
	if turnID == "" {
		return nil
	}
	m.dropOtherTurns(sessionID, turnID)
	items := m.pendingFor(sessionID, turnID)
	if len(items) == 0 {
		return nil
	}

	applied := 0
	dropApplied := func(err error) error {
		if applied > 0 {
			m.drop(sessionID, turnID, applied)
		}
		return err
	}

	for _, item := range items {
		pending := csession.NewEvent(sessionID, csession.ExternalInput, steeringEvent{
			Kind:   steeringKind,
			Status: "pending",
			Input:  item,
		})
		if err := sess.Append(ctx, pending); err != nil {
			return dropApplied(err)
		}

		contextEvent := csession.NewContext(sessionID, csession.ContextEntry{
			Kind:      csession.ContextKindGeneric,
			Placement: csession.ContextPlacementHistory,
			Content:   steeringContext(item),
		})
		contextEvent.Metadata = map[string]any{
			"kind":             steeringKind,
			"pending_event_id": pending.ID.String(),
		}
		if err := sess.Append(ctx, contextEvent); err != nil {
			return dropApplied(err)
		}

		if err := sess.Append(ctx, csession.NewEvent(sessionID, csession.ExternalInput, steeringEvent{
			Kind:           steeringKind,
			Status:         "consumed",
			PendingEventID: pending.ID.String(),
		})); err != nil {
			return dropApplied(err)
		}
		applied++
	}

	m.drop(sessionID, turnID, applied)
	return nil
}

func (m *steeringMutator) Effects() prompt.SideEffects {
	return prompt.SideEffects{Session: true}
}

func (m *steeringMutator) pendingFor(sessionID string, turnID string) []string {
	m.mu.Lock()
	defer m.mu.Unlock()
	items := m.pending[sessionID]
	if len(items) == 0 {
		return nil
	}
	out := make([]string, 0, len(items))
	for _, item := range items {
		if item.turnID == turnID {
			out = append(out, item.text)
		}
	}
	return out
}

func (m *steeringMutator) drop(sessionID string, turnID string, n int) {
	m.mu.Lock()
	defer m.mu.Unlock()
	items := m.pending[sessionID]
	if n <= 0 || len(items) == 0 {
		return
	}
	remaining := items[:0]
	dropped := 0
	for _, item := range items {
		if item.turnID == turnID && dropped < n {
			dropped++
			continue
		}
		remaining = append(remaining, item)
	}
	if len(remaining) == 0 {
		delete(m.pending, sessionID)
		return
	}
	m.pending[sessionID] = append([]pendingSteering(nil), remaining...)
}

func (m *steeringMutator) dropTurn(sessionID string, turnID string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.dropTurnLocked(sessionID, turnID)
}

func (m *steeringMutator) dropOtherTurns(sessionID string, turnID string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	items := m.pending[sessionID]
	if len(items) == 0 {
		return
	}
	remaining := items[:0]
	for _, item := range items {
		if item.turnID == turnID {
			remaining = append(remaining, item)
		}
	}
	if len(remaining) == 0 {
		delete(m.pending, sessionID)
		return
	}
	m.pending[sessionID] = append([]pendingSteering(nil), remaining...)
}

func (m *steeringMutator) dropTurnLocked(sessionID string, turnID string) {
	items := m.pending[sessionID]
	if len(items) == 0 {
		return
	}
	remaining := items[:0]
	for _, item := range items {
		if item.turnID != turnID {
			remaining = append(remaining, item)
		}
	}
	if len(remaining) == 0 {
		delete(m.pending, sessionID)
		return
	}
	m.pending[sessionID] = append([]pendingSteering(nil), remaining...)
}

func steeringContext(input string) string {
	return strings.TrimSpace(`<user_steering>
The user sent this while the current turn was running. Treat it as guidance for the next step if relevant, without repeating completed work.

` + strings.TrimSpace(input) + `
</user_steering>`)
}
