package agent

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"

	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

// journalActionBoundary is the runtime-owned policy and durability boundary.
// Tool implementations remain effectful executors; they do not write action
// records or decide whether an action is authorized.
type journalActionBoundary struct {
	journal     session.ActionJournal
	approvals   *ApprovalBroker
	mode        ApprovalMode
	workdir     string
	interactive bool
}

func newJournalActionBoundary(
	journal session.ActionJournal,
	approvals *ApprovalBroker,
	mode ApprovalMode,
	interactive bool,
	workdir string,
) *journalActionBoundary {
	return &journalActionBoundary{
		journal:     journal,
		approvals:   approvals,
		mode:        mode,
		interactive: interactive,
		workdir:     filepath.Clean(workdir),
	}
}

func (b *journalActionBoundary) PrepareAndAuthorize(ctx context.Context, request ActionRequest) (*ActionToken, error) {
	if !request.Required {
		return nil, nil
	}
	if b == nil || b.journal == nil {
		return nil, errors.New("external action journal is unavailable")
	}
	record, err := b.prepareRecord(request)
	if err != nil {
		return nil, err
	}
	prepared, err := b.journal.PrepareAction(ctx, record)
	if err != nil {
		return nil, fmt.Errorf("prepare action: %w", err)
	}
	if isTerminalActionState(prepared.State) {
		return nil, fmt.Errorf("action %q is already terminal as %s; explicit recovery is required", prepared.ID, prepared.State)
	}
	if prepared.State != session.ActionPrepared && prepared.State != session.ActionAuthorized {
		return nil, fmt.Errorf("action %q is already in %s; explicit recovery is required", prepared.ID, prepared.State)
	}

	requestEvent := session.ApprovalRequest{
		ActionID:    prepared.ID,
		Fingerprint: prepared.Fingerprint,
		ToolCallID:  request.InvocationID,
		ToolName:    request.ToolName,
		Category:    prepared.Category,
		Operation:   prepared.Operation,
		Resource:    firstActionResource(prepared),
		CWD:         prepared.CWD,
		Paths:       slices.Clone(prepared.Paths),
	}
	outcome := approvalOutcome{decision: session.ApprovalAllow}
	if b.approvals == nil {
		outcome = approvalOutcome{decision: session.ApprovalDeny, reason: "tool approval is unavailable in this runtime"}
	} else if request.Requirement.AlwaysConfirm {
		outcome = b.approvals.RequestForced(ctx, requestEvent)
	} else {
		outcome = b.approvals.Request(ctx, requestEvent)
	}
	if outcome.decision != session.ApprovalAllow && outcome.decision != session.ApprovalAlways {
		reason := outcome.reason
		if reason == "" {
			reason = "tool call denied by user"
		}
		if _, journalErr := b.journal.DenyAction(ctx, prepared.ID, reason); journalErr != nil {
			return nil, errors.Join(fmt.Errorf("deny action: %s", reason), fmt.Errorf("record action denial: %w", journalErr))
		}
		return nil, errors.New(reason)
	}
	authorized, err := b.journal.AuthorizeAction(ctx, prepared.ID, string(b.mode))
	if err != nil {
		return nil, fmt.Errorf("authorize action: %w", err)
	}
	return &ActionToken{ID: authorized.ID, Record: authorized}, nil
}

func (b *journalActionBoundary) Start(ctx context.Context, token *ActionToken) error {
	if token == nil {
		return nil
	}
	started, err := b.journal.StartAction(ctx, token.ID, "")
	if err != nil {
		// Do not finalize this as an ordinary failure. A storage commit can be
		// durable even when its caller observes an error; recovery must inspect
		// the journal and conservatively treat a started record as uncertain.
		return fmt.Errorf("action start outcome is uncertain: %w", err)
	}
	token.Record = started
	return nil
}

func (b *journalActionBoundary) Execute(
	ctx context.Context,
	token *ActionToken,
	invoke ActionInvoker,
	signal <-chan struct{},
	progress func(session.ToolPartial),
) (session.ToolResultMessage, error) {
	if invoke == nil {
		return session.ToolResultMessage{}, errors.New("action executor is unavailable")
	}
	if token == nil {
		return invoke(ctx, signal, progress)
	}
	if err := b.Start(ctx, token); err != nil {
		return session.ToolResultMessage{
			ToolCallID: token.Record.InvocationID,
			ToolName:   token.Record.Tool,
			Content:    []session.Content{session.TextContent{Text: err.Error()}},
			IsError:    true,
		}, err
	}
	if err := revalidateActionPreimages(token.Record); err != nil {
		boundaryErr := fmt.Errorf("action preimage validation: %w", err)
		result := session.ToolResultMessage{
			ToolCallID: token.Record.InvocationID,
			ToolName:   token.Record.Tool,
			Content:    []session.Content{session.TextContent{Text: boundaryErr.Error()}},
			IsError:    true,
		}
		finishErr := b.Finish(ctx, token, ActionResult{State: session.ActionFailed, Error: boundaryErr.Error()})
		if finishErr != nil {
			return result, errors.Join(boundaryErr, finishErr)
		}
		return result, boundaryErr
	}
	effectCtx := tool.WithActionPathGuard(ctx, token.Record.Paths)
	var processRecordErr error
	var backgroundMu sync.Mutex
	backgroundJob := false
	effectCtx = tool.WithProcessGroupRecorder(effectCtx, func(pid int) error {
		started, err := b.journal.StartAction(ctx, token.ID, strconv.Itoa(pid))
		if err != nil {
			processRecordErr = err
			return err
		}
		token.Record = started
		return nil
	})
	effectCtx = tool.WithJobLifecycleRecorder(effectCtx, tool.JobLifecycleRecorder{
		Started: func(string) error {
			backgroundMu.Lock()
			backgroundJob = true
			backgroundMu.Unlock()
			return nil
		},
		Finished: func(output string, err error) {
			result := ActionResult{State: session.ActionCompleted, ResultIdentity: stringResultIdentity(output)}
			if err != nil {
				result.State = session.ActionIndeterminate
				result.Error = fmt.Sprintf("background action outcome is indeterminate: %v", err)
				result.CleanupOutcome = "background process group was reaped; verify external effects before retry"
			}
			_ = b.Finish(context.Background(), token, result)
		},
	})
	result, executeErr := invoke(effectCtx, signal, progress)
	backgroundMu.Lock()
	background := backgroundJob
	backgroundMu.Unlock()
	if background {
		// The job lifecycle callback owns terminal journal finalization.
		return result, executeErr
	}
	actionResult := ActionResult{
		State:          session.ActionCompleted,
		ResultIdentity: toolResultIdentity(result),
	}
	if executeErr != nil || result.IsError {
		// Once the executor has crossed the start boundary, an error is not
		// proof that no external effect occurred. Keep the record explicitly
		// recoverable until a verifier establishes the outcome.
		actionResult.State = session.ActionIndeterminate
		if executeErr != nil {
			actionResult.Error = fmt.Sprintf("action outcome is indeterminate: %v", executeErr)
		} else {
			actionResult.Error = fmt.Sprintf("action outcome is indeterminate: %s", firstToolResultText(result))
		}
		actionResult.CleanupOutcome = "effect may have occurred; verification is required before retry"
	}
	if processRecordErr != nil {
		actionResult.State = session.ActionIndeterminate
		actionResult.Error = fmt.Sprintf("process group identity was not durably recorded: %v", processRecordErr)
		actionResult.CleanupOutcome = "executor terminated process after identity recording failure"
	}
	if finishErr := b.Finish(ctx, token, actionResult); finishErr != nil {
		if executeErr != nil {
			return result, errors.Join(executeErr, finishErr)
		}
		return result, finishErr
	}
	return result, executeErr
}

func mutatingAction(requirement ApprovalRequirement, operation string) bool {
	category := strings.ToLower(strings.TrimSpace(requirement.Category))
	operation = strings.ToLower(strings.TrimSpace(operation))
	return category == "write" || operation == "write" || operation == "edit"
}

func (b *journalActionBoundary) Finish(ctx context.Context, token *ActionToken, result ActionResult) error {
	if token == nil {
		return nil
	}
	if result.State == "" {
		result.State = session.ActionCompleted
		if result.Error != "" {
			result.State = session.ActionFailed
		}
	}
	if _, err := b.journal.FinishAction(ctx, token.ID, result.State, result.ResultIdentity, result.Error, result.CleanupOutcome); err != nil {
		return fmt.Errorf("finish action: %w", err)
	}
	return nil
}

func (b *journalActionBoundary) Cancel(ctx context.Context, token *ActionToken, reason string) error {
	if token == nil {
		return nil
	}
	return b.Finish(ctx, token, ActionResult{State: session.ActionCancelled, Error: reason})
}

func (b *journalActionBoundary) prepareRecord(request ActionRequest) (session.ActionRecord, error) {
	if strings.TrimSpace(request.ToolName) == "" {
		return session.ActionRecord{}, errors.New("action tool name is required")
	}
	if strings.TrimSpace(request.InvocationID) == "" {
		return session.ActionRecord{}, errors.New("action invocation ID is required")
	}
	if strings.TrimSpace(request.SessionID) == "" {
		return session.ActionRecord{}, errors.New("action session ID is required")
	}
	if strings.TrimSpace(request.TurnID) == "" {
		return session.ActionRecord{}, errors.New("action turn ID is required")
	}
	operation := strings.TrimSpace(request.Requirement.Operation)
	if operation == "" {
		return session.ActionRecord{}, errors.New("action operation is required")
	}
	arguments, err := canonicalJSON(request.Arguments)
	if err != nil {
		return session.ActionRecord{}, fmt.Errorf("normalize action arguments: %w", err)
	}
	cwd, err := b.canonicalWorkdir(request.CWD)
	if err != nil {
		return session.ActionRecord{}, err
	}
	paths, err := b.canonicalPaths(cwd, request.Requirement, operation)
	if err != nil {
		return session.ActionRecord{}, err
	}
	preimages := []byte(`[]`)
	if mutatingAction(request.Requirement, operation) {
		if len(paths) == 0 {
			return session.ActionRecord{}, errors.New("mutation action target is required")
		}
		preimages, err = captureActionPreimages(paths)
		if err != nil {
			return session.ActionRecord{}, err
		}
	}
	metadataValue := request.Requirement.Metadata
	if metadataValue == nil {
		metadataValue = map[string]any{}
	}
	metadataRaw, err := json.Marshal(metadataValue)
	if err != nil {
		return session.ActionRecord{}, fmt.Errorf("encode action metadata: %w", err)
	}
	metadata, err := canonicalJSON(metadataRaw)
	if err != nil {
		return session.ActionRecord{}, fmt.Errorf("normalize action metadata: %w", err)
	}
	environment := slices.Clone(request.Requirement.Environment)
	networkIntent := strings.TrimSpace(request.Requirement.NetworkIntent)
	mcpIdentity := strings.TrimSpace(request.Requirement.MCPIdentity)
	identity := struct {
		Tool          string          `json:"tool"`
		Category      string          `json:"category"`
		Operation     string          `json:"operation"`
		Arguments     json.RawMessage `json:"arguments"`
		Metadata      json.RawMessage `json:"metadata"`
		Preimages     json.RawMessage `json:"preimages"`
		CWD           string          `json:"cwd"`
		Paths         []string        `json:"paths"`
		Environment   []string        `json:"environment"`
		NetworkIntent string          `json:"network_intent"`
		MCPIdentity   string          `json:"mcp_identity"`
		PolicyMode    string          `json:"policy_mode"`
	}{
		Tool: request.ToolName, Category: strings.TrimSpace(request.Requirement.Category),
		Operation: operation, Arguments: arguments, Metadata: metadata, Preimages: preimages, CWD: cwd, Paths: paths,
		Environment: environment, NetworkIntent: networkIntent,
		MCPIdentity: mcpIdentity, PolicyMode: string(b.mode),
	}
	fingerprintPayload, err := json.Marshal(identity)
	if err != nil {
		return session.ActionRecord{}, fmt.Errorf("encode action identity: %w", err)
	}
	digest := sha256.Sum256(fingerprintPayload)
	fingerprint := "sha256:" + hex.EncodeToString(digest[:])
	actionIdentityPayload, err := json.Marshal(struct {
		SessionID    string `json:"session_id"`
		TurnID       string `json:"turn_id"`
		InvocationID string `json:"invocation_id"`
		Fingerprint  string `json:"fingerprint"`
	}{
		SessionID: request.SessionID, TurnID: request.TurnID,
		InvocationID: request.InvocationID, Fingerprint: fingerprint,
	})
	if err != nil {
		return session.ActionRecord{}, fmt.Errorf("encode action identity: %w", err)
	}
	actionDigest := sha256.Sum256(actionIdentityPayload)
	actionID := "action-" + hex.EncodeToString(actionDigest[:16])
	return session.ActionRecord{
		ID:            actionID,
		InvocationID:  request.InvocationID,
		SessionID:     request.SessionID,
		TurnID:        request.TurnID,
		Tool:          request.ToolName,
		Category:      strings.TrimSpace(request.Requirement.Category),
		Operation:     operation,
		Arguments:     arguments,
		Metadata:      metadata,
		Preimages:     preimages,
		Fingerprint:   fingerprint,
		CWD:           cwd,
		Paths:         paths,
		Environment:   environment,
		NetworkIntent: networkIntent,
		MCPIdentity:   mcpIdentity,
		PolicyMode:    string(b.mode),
		State:         session.ActionPrepared,
	}, nil
}

func canonicalJSON(raw []byte) ([]byte, error) {
	decoder := json.NewDecoder(strings.NewReader(string(raw)))
	decoder.UseNumber()
	var value any
	if err := decoder.Decode(&value); err != nil {
		return nil, err
	}
	var trailing any
	if err := decoder.Decode(&trailing); err != io.EOF {
		if err == nil {
			return nil, errors.New("trailing JSON value")
		}
		return nil, fmt.Errorf("decode trailing JSON: %w", err)
	}
	return json.Marshal(value)
}

func isTerminalActionState(state session.ActionState) bool {
	switch state {
	case session.ActionCompleted, session.ActionFailed, session.ActionCancelled,
		session.ActionDenied, session.ActionIndeterminate:
		return true
	default:
		return false
	}
}

func (b *journalActionBoundary) canonicalWorkdir(requestCWD string) (string, error) {
	root := strings.TrimSpace(requestCWD)
	if root == "" {
		root = b.workdir
	}
	if root == "" {
		return "", errors.New("action working directory is required")
	}
	abs, err := filepath.Abs(root)
	if err != nil {
		return "", fmt.Errorf("resolve action working directory: %w", err)
	}
	abs = filepath.Clean(abs)
	resolved, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return "", fmt.Errorf("resolve action working directory: %w", err)
	}
	return filepath.Clean(resolved), nil
}

func (b *journalActionBoundary) canonicalPaths(cwd string, requirement ApprovalRequirement, operation string) ([]string, error) {
	paths := slices.Clone(requirement.Paths)
	if len(paths) == 0 && (strings.EqualFold(operation, "write") || strings.EqualFold(operation, "edit")) && requirement.Resource != "" {
		paths = []string{requirement.Resource}
	}
	canonical := make([]string, 0, len(paths))
	for _, path := range paths {
		resolved, err := canonicalWorkspacePath(cwd, path)
		if err != nil {
			return nil, err
		}
		canonical = append(canonical, resolved)
	}
	return canonical, nil
}

func canonicalWorkspacePath(root, raw string) (string, error) {
	if strings.TrimSpace(raw) == "" {
		return "", errors.New("action path is empty")
	}
	path := raw
	if !filepath.IsAbs(path) {
		path = filepath.Join(root, path)
	}
	path = filepath.Clean(path)
	rel, err := filepath.Rel(root, path)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("action path %q escapes workspace %q", raw, root)
	}
	resolved, err := resolvePathWithMissingLeaf(path)
	if err != nil {
		return "", fmt.Errorf("resolve action path %q: %w", raw, err)
	}
	resolvedRel, err := filepath.Rel(root, resolved)
	if err != nil || resolvedRel == ".." || strings.HasPrefix(resolvedRel, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("action path %q resolves outside workspace", raw)
	}
	return resolved, nil
}

func resolvePathWithMissingLeaf(path string) (string, error) {
	current := path
	var suffix []string
	for {
		_, err := os.Lstat(current)
		if err == nil {
			resolved, err := filepath.EvalSymlinks(current)
			if err != nil {
				return "", err
			}
			for i := len(suffix) - 1; i >= 0; i-- {
				resolved = filepath.Join(resolved, suffix[i])
			}
			return filepath.Clean(resolved), nil
		}
		if !errors.Is(err, os.ErrNotExist) {
			return "", err
		}
		parent := filepath.Dir(current)
		if parent == current {
			return "", os.ErrNotExist
		}
		suffix = append(suffix, filepath.Base(current))
		current = parent
	}
}

func firstActionResource(record session.ActionRecord) string {
	if len(record.Paths) > 0 {
		return record.Paths[0]
	}
	return record.Operation
}

func stringResultIdentity(value string) string {
	digest := sha256.Sum256([]byte(value))
	return "sha256:" + hex.EncodeToString(digest[:])
}
