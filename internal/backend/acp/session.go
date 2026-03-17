package acp

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"sync"

	"github.com/go-json-experiment/json"
	"github.com/go-json-experiment/json/jsontext"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type Session struct {
	events  chan session.Event
	store   storage.Store
	storage storage.Session
	
	cmd    *exec.Cmd
	stdin  io.WriteCloser
	cancel context.CancelFunc
	mu     sync.Mutex
}

func newSession() *Session {
	return &Session{
		events: make(chan session.Event, 100),
	}
}

func (s *Session) Open(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	command := os.Getenv("ION_ACP_COMMAND")
	if command == "" {
		return fmt.Errorf("ION_ACP_COMMAND environment variable not set")
	}

	// Create a cancelable context for the process
	ctx, cancel := context.WithCancel(ctx)
	s.cancel = cancel

	// Start the external agent process
	cmd := exec.CommandContext(ctx, "bash", "-c", command)
	s.cmd = cmd

	stdin, err := cmd.StdinPipe()
	if err != nil {
		return fmt.Errorf("failed to open stdin: %w", err)
	}
	s.stdin = stdin

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("failed to open stdout: %w", err)
	}

	stderr, err := cmd.StderrPipe()
	if err != nil {
		return fmt.Errorf("failed to open stderr: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return fmt.Errorf("failed to start agent: %w", err)
	}

	var wg sync.WaitGroup

	// Stream stdout (ACP Events)
	wg.Go(func() {
		scanner := bufio.NewScanner(stdout)
		for scanner.Scan() {
			var ev Wrapper
			if err := json.Unmarshal(scanner.Bytes(), &ev); err != nil {
				s.events <- session.Error{Err: fmt.Errorf("failed to parse ACP event: %w", err)}
				continue
			}
			
			// Map ACP wrapper to session.Event
			if typedEv := ev.ToEvent(); typedEv != nil {
				s.events <- typedEv
			}
		}
	})

	// Stream stderr to session errors (non-fatal)
	wg.Go(func() {
		scanner := bufio.NewScanner(stderr)
		for scanner.Scan() {
			s.events <- session.Error{Err: fmt.Errorf("agent stderr: %s", scanner.Text())}
		}
	})

	// Background wait to clean up
	go func() {
		wg.Wait()
		_ = cmd.Wait() // Ensure process is reaped
	}()

	return nil
}

func (s *Session) Resume(ctx context.Context, sessionID string) error {
	// For now, Resume is similar to Open but might pass different flags to the external agent
	return s.Open(ctx)
}

func (s *Session) SubmitTurn(ctx context.Context, input string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.stdin == nil {
		return fmt.Errorf("agent not connected")
	}

	// Send turn request to external agent
	req := Request{
		Type:  "submit",
		Input: input,
	}
	
	bytes, err := json.Marshal(req)
	if err != nil {
		return err
	}

	_, err = s.stdin.Write(append(bytes, '\n'))
	return err
}

func (s *Session) CancelTurn(ctx context.Context) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.stdin == nil {
		return nil
	}

	// Send cancel request
	req := Request{Type: "cancel"}
	bytes, _ := json.Marshal(req)
	_, err := s.stdin.Write(append(bytes, '\n'))
	return err
}

func (s *Session) Approve(ctx context.Context, requestID string, approved bool) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.stdin == nil {
		return fmt.Errorf("agent not connected")
	}

	// Send approval response
	req := Request{
		Type:      "approve",
		RequestID: requestID,
		Approved:  approved,
	}

	bytes, err := json.Marshal(req)
	if err != nil {
		return err
	}

	_, err = s.stdin.Write(append(bytes, '\n'))
	return err
}

func (s *Session) Close() error {
	if s.cancel != nil {
		s.cancel()
	}
	if s.stdin != nil {
		s.stdin.Close()
	}
	close(s.events)
	return nil
}

func (s *Session) Events() <-chan session.Event {
	return s.events
}

func (s *Session) ID() string {
	if s.storage != nil {
		return s.storage.ID()
	}
	return ""
}

func (s *Session) Meta() map[string]string {
	if s.storage != nil {
		m := s.storage.Meta()
		return map[string]string{
			"model":  m.Model,
			"branch": m.Branch,
			"cwd":    m.CWD,
		}
	}
	return nil
}

// ACP Protocol Types

type Request struct {
	Type      string `json:"type"`
	Input     string `json:"input,omitempty"`
	RequestID string `json:"request_id,omitempty"`
	Approved  bool   `json:"approved,omitempty"`
}

type Wrapper struct {
	Type string         `json:"type"`
	Data jsontext.Value `json:"data"`
}

func (w Wrapper) ToEvent() session.Event {
	switch w.Type {
	case "status_changed":
		var e session.StatusChanged
		_ = json.Unmarshal(w.Data, &e)
		return e
	case "assistant_delta":
		var e session.AssistantDelta
		_ = json.Unmarshal(w.Data, &e)
		return e
	case "thinking_delta":
		var e session.ThinkingDelta
		_ = json.Unmarshal(w.Data, &e)
		return e
	case "tool_call_started":
		var e session.ToolCallStarted
		_ = json.Unmarshal(w.Data, &e)
		return e
	case "tool_result":
		var e session.ToolResult
		_ = json.Unmarshal(w.Data, &e)
		return e
	case "assistant_message":
		var e session.AssistantMessage
		_ = json.Unmarshal(w.Data, &e)
		return e
	case "tool_output_delta":
		var e session.ToolOutputDelta
		_ = json.Unmarshal(w.Data, &e)
		return e
	case "verification_result":
		var e session.VerificationResult
		_ = json.Unmarshal(w.Data, &e)
		return e
	case "approval_request":
		var e session.ApprovalRequest
		_ = json.Unmarshal(w.Data, &e)
		return e
	case "turn_started":
		return session.TurnStarted{}
	case "turn_finished":
		return session.TurnFinished{}
	default:
		return nil
	}
}
