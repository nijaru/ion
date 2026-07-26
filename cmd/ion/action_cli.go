package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"strings"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

type actionCommandError struct {
	JSON bool
	err  error
}

func (e *actionCommandError) Error() string { return e.err.Error() }
func (e *actionCommandError) Unwrap() error { return e.err }

type actionCommandView struct {
	ID             string                      `json:"id"`
	Tool           string                      `json:"tool"`
	Category       string                      `json:"category,omitempty"`
	Operation      string                      `json:"operation,omitempty"`
	State          session.ActionState         `json:"state"`
	Authorization  session.ActionAuthorization `json:"authorization,omitempty"`
	Fingerprint    string                      `json:"fingerprint,omitempty"`
	CWD            string                      `json:"cwd,omitempty"`
	Paths          []string                    `json:"paths,omitempty"`
	Environment    []string                    `json:"environment,omitempty"`
	NetworkIntent  string                      `json:"network_intent,omitempty"`
	MCPIdentity    string                      `json:"mcp_identity,omitempty"`
	PolicyMode     string                      `json:"policy_mode,omitempty"`
	ResultIdentity string                      `json:"result_identity,omitempty"`
	Error          string                      `json:"error,omitempty"`
	Cleanup        string                      `json:"cleanup,omitempty"`
}

func actionCommandViewOf(action session.ActionRecord) actionCommandView {
	return actionCommandView{
		ID: action.ID, Tool: action.Tool, Category: action.Category,
		Operation: action.Operation, State: action.State,
		Authorization: action.Authorization, Fingerprint: action.Fingerprint,
		CWD: action.CWD, Paths: append([]string(nil), action.Paths...),
		Environment:   append([]string(nil), action.Environment...),
		NetworkIntent: action.NetworkIntent, MCPIdentity: action.MCPIdentity,
		PolicyMode: action.PolicyMode, ResultIdentity: action.ResultIdentity,
		Error: action.Error, Cleanup: action.CleanupOutcome,
	}
}

func runActionCommand(args []string, stdout io.Writer) (err error) {
	flagSet := flag.NewFlagSet("ion actions", flag.ContinueOnError)
	flagSet.SetOutput(io.Discard)
	jsonOutput := flagSet.Bool("json", false, "Emit a redacted JSON result")
	sessionDir := flagSet.String("session-dir", "", "Session storage directory")
	if err := flagSet.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return actionCommandFailure(*jsonOutput, errors.New(actionCommandUsage()))
		}
		return actionCommandFailure(*jsonOutput, err)
	}
	args = flagSet.Args()
	if len(args) == 0 {
		return actionCommandFailure(*jsonOutput, errors.New(actionCommandUsage()))
	}

	ctx := context.Background()
	store, err := openStartupStore(false, *sessionDir)
	if err != nil {
		return actionCommandFailure(*jsonOutput, fmt.Errorf("open session storage: %w", err))
	}
	defer func() {
		if closeErr := store.Close(); closeErr != nil {
			closeFailure := actionCommandFailure(*jsonOutput, fmt.Errorf("close session storage: %w", closeErr))
			if err == nil {
				err = closeFailure
			} else {
				err = errors.Join(err, closeFailure)
			}
		}
	}()

	workdir, err := os.Getwd()
	if err != nil {
		return actionCommandFailure(*jsonOutput, fmt.Errorf("resolve working directory: %w", err))
	}
	durable, ok := any(store).(session.DurableStore)
	if !ok {
		return actionCommandFailure(*jsonOutput, errors.New("session storage does not support durable turns"))
	}
	journal, ok := any(store).(session.ActionJournal)
	if !ok {
		return actionCommandFailure(*jsonOutput, errors.New("session storage does not support action journaling"))
	}
	runtime := agent.NewController(agent.ControllerConfig{
		Session:             session.NewSession(store, 64),
		Store:               store,
		Durable:             durable,
		RequireDurable:      true,
		Model:               llm.Model{ID: "recovery"},
		ApprovalMode:        agent.ApprovalConfirm,
		ApprovalInteractive: false,
		ActionJournal:       journal,
		Workdir:             workdir,
		ProcessReconciler:   tool.NewProcessReconciler(),
	})
	defer func() {
		if closeErr := runtime.Close(); closeErr != nil {
			closeFailure := actionCommandFailure(*jsonOutput, fmt.Errorf("close recovery runtime: %w", closeErr))
			if err == nil {
				err = closeFailure
			} else {
				err = errors.Join(err, closeFailure)
			}
		}
	}()
	recovery := agent.ActionRecovery(runtime)
	processRecovery, ok := any(runtime).(agent.ProcessRecovery)
	if !ok {
		return actionCommandFailure(*jsonOutput, errors.New("runtime does not support process-backed action recovery"))
	}
	if err := processRecovery.RecoverProcessActions(ctx); err != nil {
		return actionCommandFailure(*jsonOutput, fmt.Errorf("reconcile process-backed actions: %w", err))
	}
	unsettled, err := recovery.UnsettledActions(ctx)
	if err != nil {
		return actionCommandFailure(*jsonOutput, fmt.Errorf("load unsettled actions: %w", err))
	}

	switch args[0] {
	case "list":
		if len(args) != 1 {
			return actionCommandFailure(*jsonOutput, errors.New(actionCommandUsage()))
		}
		return writeActionList(stdout, unsettled, *jsonOutput)
	case "reconcile":
		if len(args) < 4 {
			return actionCommandFailure(*jsonOutput, errors.New(actionCommandUsage()))
		}
		state, err := parseActionReconcileState(args[2])
		if err != nil {
			return actionCommandFailure(*jsonOutput, err)
		}
		evidence := strings.TrimSpace(strings.Join(args[3:], " "))
		if evidence == "" {
			return actionCommandFailure(*jsonOutput, errors.New("reconciliation evidence is required"))
		}
		for _, action := range unsettled {
			if action.ID != args[1] {
				continue
			}
			if action.State != session.ActionIndeterminate {
				return actionCommandFailure(
					*jsonOutput,
					fmt.Errorf(
						"action %q is %s; only indeterminate actions can be reconciled",
						action.ID,
						action.State,
					),
				)
			}
			reconciled, err := recovery.ReconcileAction(ctx, action.ID, state, evidence, "", "", "")
			if err != nil {
				return actionCommandFailure(*jsonOutput, fmt.Errorf("reconcile action: %w", err))
			}
			return writeActionResult(stdout, reconciled, *jsonOutput)
		}
		return actionCommandFailure(
			*jsonOutput,
			fmt.Errorf("action %q is not an unsettled action; run 'ion actions list'", args[1]),
		)
	default:
		return actionCommandFailure(*jsonOutput, errors.New(actionCommandUsage()))
	}
}

func actionCommandFailure(jsonOutput bool, err error) error {
	if err == nil {
		err = errors.New("action command failed")
	}
	return &actionCommandError{JSON: jsonOutput, err: err}
}

func parseActionReconcileState(value string) (session.ActionState, error) {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case string(session.ActionCompleted):
		return session.ActionCompleted, nil
	case string(session.ActionFailed):
		return session.ActionFailed, nil
	default:
		return "", errors.New("reconciliation outcome must be completed or failed")
	}
}

func writeActionList(w io.Writer, actions []session.ActionRecord, jsonOutput bool) error {
	views := make([]actionCommandView, 0, len(actions))
	for _, action := range actions {
		views = append(views, actionCommandViewOf(action))
	}
	if jsonOutput {
		return json.NewEncoder(w).Encode(struct {
			Actions []actionCommandView `json:"actions"`
		}{Actions: views})
	}
	if len(actions) == 0 {
		_, err := io.WriteString(w, "No unsettled external actions.\n")
		return err
	}
	if _, err := fmt.Fprintf(w, "Unsettled external actions: %d\n", len(actions)); err != nil {
		return err
	}
	for _, action := range views {
		if _, err := fmt.Fprintf(w, "- %s: %s %s", action.ID, action.Tool, action.State); err != nil {
			return err
		}
		if action.Error != "" {
			if _, err := fmt.Fprintf(w, " — %s", strings.Join(strings.Fields(action.Error), " ")); err != nil {
				return err
			}
		}
		if _, err := fmt.Fprintln(w); err != nil {
			return err
		}
	}
	return nil
}

func writeActionResult(w io.Writer, action session.ActionRecord, jsonOutput bool) error {
	view := actionCommandViewOf(action)
	if jsonOutput {
		return json.NewEncoder(w).Encode(struct {
			Action actionCommandView `json:"action"`
		}{Action: view})
	}
	_, err := fmt.Fprintf(w, "Reconciled action %s as %s.\n", view.ID, view.State)
	return err
}

func actionCommandUsage() string {
	return strings.Join([]string{
		"usage:",
		"  ion actions [--json] [--session-dir <path>] list",
		"  ion actions [--json] [--session-dir <path>] reconcile <action-id> <completed|failed> <evidence>",
	}, "\n")
}
