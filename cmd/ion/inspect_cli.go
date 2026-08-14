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

	"github.com/nijaru/ion/session"
)

type TrajectoryTurnView struct {
	TurnID    string              `json:"turn_id"`
	TurnState string              `json:"turn_state"`
	StartedAt string              `json:"started_at,omitempty"`
	EndedAt   string              `json:"ended_at,omitempty"`
	InputText string              `json:"input_text,omitempty"`
	Messages  []TrajectoryMsgView `json:"messages"`
}

type TrajectoryMsgView struct {
	ID        string `json:"id"`
	Role      string `json:"role"`
	Summary   string `json:"summary"`
	Timestamp string `json:"timestamp,omitempty"`
}

type TrajectoryReport struct {
	SessionID string               `json:"session_id"`
	Workdir   string               `json:"workdir"`
	LeafEntry string               `json:"leaf_entry"`
	Turns     []TrajectoryTurnView `json:"turns"`
}

func inspectCommandUsage() string {
	return `Usage: ion inspect <subcommand> [flags]

Subcommands:
  trajectory [session-id] [--json] [--session-dir <dir>]    Inspect model-visible trajectory and turn provenance
`
}

func runInspectCommand(args []string, stdout io.Writer) error {
	if len(args) == 0 {
		return errors.New(inspectCommandUsage())
	}
	switch args[0] {
	case "trajectory":
		return runInspectTrajectory(args[1:], stdout)
	default:
		return errors.New(inspectCommandUsage())
	}
}

func runInspectTrajectory(args []string, stdout io.Writer) (err error) {
	flagSet := flag.NewFlagSet("ion inspect trajectory", flag.ContinueOnError)
	flagSet.SetOutput(io.Discard)
	jsonOutput := flagSet.Bool("json", false, "Emit JSON formatted trajectory")
	sessionDir := flagSet.String("session-dir", "", "Session storage directory")
	if err := flagSet.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			_, err = fmt.Fprintln(stdout, "Usage: ion inspect trajectory [session-id] [--json] [--session-dir <dir>]")
			return err
		}
		return err
	}

	workdir, err := os.Getwd()
	if err != nil {
		return fmt.Errorf("resolve working directory: %w", err)
	}

	store, err := openStartupStore(false, *sessionDir)
	if err != nil {
		return fmt.Errorf("open session storage: %w", err)
	}
	defer func() {
		if closeErr := store.Close(); closeErr != nil {
			if err == nil {
				err = closeErr
			} else {
				err = errors.Join(err, closeErr)
			}
		}
	}()

	ctx := context.Background()
	leafID := store.GetLeafID()
	entries, err := store.Branch(ctx)
	if err != nil {
		return fmt.Errorf("read session branch: %w", err)
	}

	var msgViews []TrajectoryMsgView
	for _, e := range entries {
		switch entry := e.(type) {
		case *session.MessageEntry:
			msg := entry.Message
			role := "message"
			switch msg.(type) {
			case *session.UserMessage:
				role = "user"
			case *session.AssistantMessage:
				role = "assistant"
			case *session.ToolResultMessage:
				role = "tool_result"
			}
			text := session.MessageText(msg)
			if len(text) > 80 {
				text = text[:77] + "..."
			}
			msgViews = append(msgViews, TrajectoryMsgView{
				ID:        entry.ID(),
				Role:      role,
				Summary:   fmt.Sprintf("%s: %s", role, text),
				Timestamp: entry.When().Format("2006-01-02 15:04:05"),
			})
		case *session.ModelChangeEntry:
			msgViews = append(msgViews, TrajectoryMsgView{
				ID:        entry.ID(),
				Role:      "model_change",
				Summary:   fmt.Sprintf("model changed to %s/%s", entry.Provider, entry.ModelID),
				Timestamp: entry.When().Format("2006-01-02 15:04:05"),
			})
		case *session.CompactionEntry:
			msgViews = append(msgViews, TrajectoryMsgView{
				ID:        entry.ID(),
				Role:      "compaction",
				Summary:   fmt.Sprintf("compaction: %s (tokens before: %d)", entry.Summary, entry.TokensBefore),
				Timestamp: entry.When().Format("2006-01-02 15:04:05"),
			})
		case *session.BranchSummaryEntry:
			msgViews = append(msgViews, TrajectoryMsgView{
				ID:        entry.ID(),
				Role:      "branch_summary",
				Summary:   fmt.Sprintf("branch summary: %s", entry.Summary),
				Timestamp: entry.When().Format("2006-01-02 15:04:05"),
			})
		default:
			msgViews = append(msgViews, TrajectoryMsgView{
				ID:        entry.ID(),
				Role:      "entry",
				Summary:   fmt.Sprintf("entry %s", entry.ID()),
				Timestamp: entry.When().Format("2006-01-02 15:04:05"),
			})
		}
	}

	report := TrajectoryReport{
		SessionID: store.Meta().ID,
		Workdir:   workdir,
		LeafEntry: leafID,
		Turns: []TrajectoryTurnView{
			{
				TurnID:    "latest",
				TurnState: "committed",
				Messages:  msgViews,
			},
		},
	}

	if *jsonOutput {
		enc := json.NewEncoder(stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(report)
	}

	var b strings.Builder
	b.WriteString("=== Ion Trajectory & Provenance Inspection ===\n")
	b.WriteString(fmt.Sprintf("Session:    %s\n", report.SessionID))
	b.WriteString(fmt.Sprintf("Workspace:  %s\n", report.Workdir))
	b.WriteString(fmt.Sprintf("Leaf Entry: %s\n", report.LeafEntry))
	b.WriteString(fmt.Sprintf("Total Messages on Branch: %d\n\n", len(msgViews)))

	for _, m := range msgViews {
		b.WriteString(fmt.Sprintf("  [%s] %s\n", m.Timestamp, m.Summary))
	}

	_, err = fmt.Fprint(stdout, b.String())
	return err
}
