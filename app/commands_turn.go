package app

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

type interruptedTurnAbortedMsg struct {
	generation uint64
	requestID  uint64
	turn       session.TurnRecord
	err        error
}

func (m Model) handleTurnsCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) == 1 {
		if len(m.Model.InterruptedTurns) == 0 {
			return m, m.terminalCommit().Entries(systemEntry("No interrupted turns."))
		}
		return m, m.terminalCommit().Entries(systemEntry(interruptedTurnSummary(m.Model.InterruptedTurns)))
	}
	if len(fields) != 3 || strings.ToLower(fields[1]) != "abort" {
		return m, cmdError("usage: /turns [abort <turn-id>]")
	}
	if m.Model.InterruptedTurnRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("discarding an interrupted turn"))
	}
	turnID := strings.TrimSpace(fields[2])
	if turnID == "" {
		return m, cmdError("turn ID is required")
	}
	var known bool
	for _, turn := range m.Model.InterruptedTurns {
		if turn.ID == turnID {
			known = true
			break
		}
	}
	if !known {
		return m, cmdError(fmt.Sprintf("turn %q is not an interrupted turn; run /turns", turnID))
	}
	recovery, ok := m.Model.Runner.(agent.TurnRecovery)
	if !ok {
		return m, cmdError("active runtime does not support interrupted-turn recovery")
	}
	m.Model.InterruptedTurnRequest++
	requestID := m.Model.InterruptedTurnRequest
	generation := m.Model.EventGeneration
	ctx := m.runtimeOperationContext()
	return m, func() tea.Msg {
		turn, err := recovery.AbortInterruptedTurn(ctx, turnID, "user discarded interrupted turn")
		return interruptedTurnAbortedMsg{
			generation: generation,
			requestID:  requestID,
			turn:       turn,
			err:        err,
		}
	}
}

func (m Model) handleInterruptedTurnAborted(msg interruptedTurnAbortedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || msg.requestID != m.Model.InterruptedTurnRequest {
		return m, nil
	}
	m.Model.InterruptedTurnRequest = 0
	if msg.err != nil {
		return m, cmdError(fmt.Sprintf("discard interrupted turn: %v", msg.err))
	}
	remaining := m.Model.InterruptedTurns[:0]
	for _, turn := range m.Model.InterruptedTurns {
		if turn.ID != msg.turn.ID {
			remaining = append(remaining, turn)
		}
	}
	m.Model.InterruptedTurns = remaining
	return m, m.terminalCommit().Entries(systemEntry(fmt.Sprintf(
		"Discarded interrupted turn %s. Its input and staged entries remain excluded from replay.", msg.turn.ID,
	)))
}
