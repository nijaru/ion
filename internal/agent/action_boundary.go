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
	"sort"
	"strings"
	"sync"

	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

// journalActionBoundary is the runtime-owned policy and durability boundary.
// Tool implementations remain effectful executors; they do not write action
// records or decide whether an action is authorized.
type journalActionBoundary struct {
	controller  *Controller
	journal     session.ActionJournal
	approvals   *ApprovalBroker
	mode        ApprovalMode
	workdir     string
	interactive bool
	pathLocks   *actionPathLocks
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
		pathLocks:   newActionPathLocks(),
	}
}

func newControllerActionCoordinator(
	controller *Controller,
	journal session.ActionJournal,
	approvals *ApprovalBroker,
	mode ApprovalMode,
	interactive bool,
	workdir string,
) *journalActionBoundary {
	boundary := newJournalActionBoundary(journal, approvals, mode, interactive, workdir)
	boundary.controller = controller
	return boundary
}

func (b *journalActionBoundary) PrepareAndAuthorize(ctx context.Context, request ActionRequest) (*ActionToken, error) {
	if b != nil && b.controller != nil {
		request = cloneActionRequest(request)
		reply := make(chan ActionPrepareResult, 1)
		if err := b.controller.enqueue(ctx, &PrepareActionCmd{Ctx: ctx, Request: request, Reply: reply}); err != nil {
			return nil, err
		}
		result, err := waitCommandReply(ctx, reply)
		if err != nil {
			return nil, err
		}
		return result.Token, result.Err
	}
	return b.prepareAndAuthorizeDirect(ctx, request)
}

func (b *journalActionBoundary) prepareAndAuthorizeDirect(
	ctx context.Context,
	request ActionRequest,
) (*ActionToken, error) {
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
		return nil, fmt.Errorf(
			"action %q is already terminal as %s; explicit recovery is required",
			prepared.ID,
			prepared.State,
		)
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
		outcome = approvalOutcome{
			decision: session.ApprovalDeny,
			reason:   "tool approval is unavailable in this runtime",
		}
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
			return nil, errors.Join(
				fmt.Errorf("deny action: %s", reason),
				fmt.Errorf("record action denial: %w", journalErr),
			)
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
	return b.start(ctx, token, "")
}

func (b *journalActionBoundary) start(ctx context.Context, token *ActionToken, processIdentity string) error {
	if token == nil {
		return nil
	}
	if b != nil && b.controller != nil {
		reply := make(chan ActionStartResult, 1)
		if err := b.controller.enqueue(ctx, &StartActionCmd{
			Ctx: ctx, Token: cloneActionToken(*token), ProcessIdentity: processIdentity, Reply: reply,
		}); err != nil {
			return err
		}
		result, err := waitCommandReply(ctx, reply)
		if err != nil {
			return err
		}
		if result.Token != nil {
			*token = *result.Token
		}
		return result.Err
	}
	return b.startDirect(ctx, token, processIdentity)
}

func (b *journalActionBoundary) startDirect(ctx context.Context, token *ActionToken, processIdentity string) error {
	if token == nil {
		return nil
	}
	started, err := b.journal.StartAction(ctx, token.ID, processIdentity)
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
	releasePathLocks := func() {}
	if mutatingActionRecord(token.Record) {
		releasePathLocks = b.pathLocks.acquire(token.Record.Paths)
	}
	defer releasePathLocks()
	if err := revalidateActionPreimages(token.Record); err != nil {
		boundaryErr := fmt.Errorf("action preimage validation: %w", err)
		result := session.ToolResultMessage{
			ToolCallID: token.Record.InvocationID,
			ToolName:   token.Record.Tool,
			Content:    []session.Content{session.TextContent{Text: boundaryErr.Error()}},
			IsError:    true,
		}
		finishErr := b.finish(ctx, token, ActionResult{State: session.ActionFailed, Error: boundaryErr.Error()})
		if finishErr != nil {
			return result, errors.Join(boundaryErr, finishErr)
		}
		return result, boundaryErr
	}
	if err := b.start(ctx, token, ""); err != nil {
		return session.ToolResultMessage{
			ToolCallID: token.Record.InvocationID,
			ToolName:   token.Record.Tool,
			Content:    []session.Content{session.TextContent{Text: err.Error()}},
			IsError:    true,
		}, err
	}
	effectCtx := tool.WithActionPathGuard(ctx, token.Record.Paths)
	var processRecordErr error
	var processRecordMu sync.Mutex
	var backgroundMu sync.Mutex
	backgroundJob := false
	effectCtx = tool.WithProcessIdentityRecorder(effectCtx, func(launch tool.ProcessLaunch) error {
		processIdentity, err := tool.CaptureProcessLaunchIdentity(launch)
		if err != nil {
			return fmt.Errorf("capture process identity: %w", err)
		}
		err = b.start(b.durableContext(ctx), token, processIdentity)
		if err != nil {
			processRecordMu.Lock()
			processRecordErr = err
			processRecordMu.Unlock()
		}
		return err
	})
	effectCtx = tool.WithJobLifecycleRecorder(effectCtx, tool.JobLifecycleRecorder{
		Started: func(string) error {
			backgroundMu.Lock()
			backgroundJob = true
			backgroundMu.Unlock()
			return nil
		},
		Finished: func(output string, err error) error {
			result := ActionResult{State: session.ActionCompleted, ResultIdentity: stringResultIdentity(output)}
			if err != nil {
				result.State = session.ActionIndeterminate
				result.Error = fmt.Sprintf("background action outcome is indeterminate: %v", err)
				result.CleanupOutcome = "background process group was reaped; verify external effects before retry"
			}
			if finishErr := b.finish(b.durableContext(context.Background()), token, result); finishErr != nil {
				return fmt.Errorf("finalize background action %q: %w", token.ID, finishErr)
			}
			return nil
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
	processRecordMu.Lock()
	processRecordFailure := processRecordErr
	processRecordMu.Unlock()
	if processRecordFailure != nil {
		actionResult.State = session.ActionIndeterminate
		actionResult.Error = fmt.Sprintf("process group identity was not durably recorded: %v", processRecordFailure)
		actionResult.CleanupOutcome = "executor terminated process after identity recording failure"
	}
	if finishErr := b.finish(b.durableContext(ctx), token, actionResult); finishErr != nil {
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

func mutatingActionRecord(record session.ActionRecord) bool {
	return mutatingAction(ApprovalRequirement{Category: record.Category}, record.Operation)
}

func (b *journalActionBoundary) Finish(ctx context.Context, token *ActionToken, result ActionResult) error {
	return b.finish(ctx, token, result)
}

func (b *journalActionBoundary) finish(ctx context.Context, token *ActionToken, result ActionResult) error {
	if token == nil {
		return nil
	}
	if result.State == "" {
		result.State = session.ActionCompleted
		if result.Error != "" {
			result.State = session.ActionFailed
		}
	}
	if b != nil && b.controller != nil {
		durableCtx := b.durableContext(ctx)
		reply := make(chan error, 1)
		if err := b.controller.enqueue(durableCtx, &FinishActionCmd{
			Ctx: durableCtx, Token: cloneActionToken(*token), Result: result, Reply: reply,
		}); err != nil {
			return err
		}
		return waitCommandReplyError(durableCtx, reply)
	}
	if _, err := b.journal.FinishAction(
		ctx,
		token.ID,
		result.State,
		result.ResultIdentity,
		result.Error,
		result.CleanupOutcome,
	); err != nil {
		return fmt.Errorf("finish action: %w", err)
	}
	return nil
}

func (b *journalActionBoundary) Cancel(ctx context.Context, token *ActionToken, reason string) error {
	if token == nil {
		return nil
	}
	if b != nil && b.controller != nil {
		durableCtx := b.durableContext(ctx)
		reply := make(chan error, 1)
		if err := b.controller.enqueue(durableCtx, &CancelActionCmd{
			Ctx: durableCtx, Token: cloneActionToken(*token), Reason: reason, Reply: reply,
		}); err != nil {
			return err
		}
		return waitCommandReplyError(durableCtx, reply)
	}
	return b.cancelDirect(ctx, token, reason)
}

func (b *journalActionBoundary) cancelDirect(ctx context.Context, token *ActionToken, reason string) error {
	if token == nil {
		return nil
	}
	state := session.ActionCancelled
	if token.Record.State == session.ActionStarted {
		state = session.ActionIndeterminate
		if reason == "" {
			reason = "action cancellation crossed the start boundary; outcome is indeterminate"
		} else {
			reason = "action cancellation crossed the start boundary: " + reason
		}
	}
	return b.finishDirect(ctx, token, ActionResult{State: state, Error: reason})
}

func (b *journalActionBoundary) finishDirect(ctx context.Context, token *ActionToken, result ActionResult) error {
	if token == nil {
		return nil
	}
	if result.State == "" {
		result.State = session.ActionCompleted
		if result.Error != "" {
			result.State = session.ActionFailed
		}
	}
	if _, err := b.journal.FinishAction(
		ctx,
		token.ID,
		result.State,
		result.ResultIdentity,
		result.Error,
		result.CleanupOutcome,
	); err != nil {
		return fmt.Errorf("finish action: %w", err)
	}
	return nil
}

func (b *journalActionBoundary) durableContext(fallback context.Context) context.Context {
	if b == nil || b.controller == nil {
		return fallback
	}
	b.controller.mu.Lock()
	ctx := b.controller.runtimeContext
	b.controller.mu.Unlock()
	if ctx != nil {
		return ctx
	}
	return fallback
}

func waitCommandReplyError(ctx context.Context, reply <-chan error) error {
	err, waitErr := waitCommandReply(ctx, reply)
	if waitErr != nil {
		return waitErr
	}
	return err
}

func cloneActionToken(token ActionToken) ActionToken {
	token.Record.Arguments = slices.Clone(token.Record.Arguments)
	token.Record.Metadata = slices.Clone(token.Record.Metadata)
	token.Record.Preimages = slices.Clone(token.Record.Preimages)
	token.Record.Paths = slices.Clone(token.Record.Paths)
	token.Record.Environment = slices.Clone(token.Record.Environment)
	return token
}

func cloneActionRequest(request ActionRequest) ActionRequest {
	request.Arguments = slices.Clone(request.Arguments)
	requirement := request.Requirement
	requirement.Paths = slices.Clone(requirement.Paths)
	requirement.Environment = slices.Clone(requirement.Environment)
	if requirement.Metadata != nil {
		requirement.Metadata = cloneActionMetadata(requirement.Metadata)
	}
	request.Requirement = requirement
	return request
}

func cloneActionMetadata(metadata map[string]any) map[string]any {
	clone := make(map[string]any, len(metadata))
	for key, value := range metadata {
		clone[key] = value
	}
	return clone
}

func (c *Controller) actionJournal() (session.ActionJournal, error) {
	if c == nil || c.store == nil {
		return nil, errors.New("session store does not support action recovery")
	}
	journal, ok := c.store.(session.ActionJournal)
	if !ok || journal == nil {
		return nil, errors.New("session store does not support action recovery")
	}
	return journal, nil
}

func (c *Controller) actionCoordinator() (*journalActionBoundary, error) {
	if c == nil || c.actionBoundary == nil {
		return nil, errors.New("external action coordinator is unavailable")
	}
	coordinator, ok := c.actionBoundary.(*journalActionBoundary)
	if !ok || coordinator == nil {
		return nil, errors.New("external action coordinator is unavailable")
	}
	return coordinator, nil
}

func (b *journalActionBoundary) prepareRecord(request ActionRequest) (session.ActionRecord, error) {
	toolName := strings.TrimSpace(request.ToolName)
	if toolName == "" {
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
	category := strings.ToLower(strings.TrimSpace(request.Requirement.Category))
	operation := strings.ToLower(strings.TrimSpace(request.Requirement.Operation))
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
	environment := canonicalStringList(request.Requirement.Environment)
	networkIntent := strings.ToLower(strings.TrimSpace(request.Requirement.NetworkIntent))
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
		Tool: toolName, Category: category,
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
		Tool:          toolName,
		Category:      category,
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
	workspace := strings.TrimSpace(b.workdir)
	if workspace == "" {
		return "", errors.New("action working directory is required")
	}
	workspaceAbs, err := filepath.Abs(workspace)
	if err != nil {
		return "", fmt.Errorf("resolve action working directory: %w", err)
	}
	workspaceResolved, err := filepath.EvalSymlinks(filepath.Clean(workspaceAbs))
	if err != nil {
		return "", fmt.Errorf("resolve action working directory: %w", err)
	}
	requested := strings.TrimSpace(requestCWD)
	if requested == "" {
		return filepath.Clean(workspaceResolved), nil
	}
	requestedAbs, err := filepath.Abs(requested)
	if err != nil {
		return "", fmt.Errorf("resolve action working directory: %w", err)
	}
	requestedResolved, err := filepath.EvalSymlinks(filepath.Clean(requestedAbs))
	if err != nil {
		return "", fmt.Errorf("resolve action working directory: %w", err)
	}
	if !pathWithin(workspaceResolved, requestedResolved) {
		return "", fmt.Errorf("action working directory %q escapes workspace %q", requestCWD, workspaceResolved)
	}
	return filepath.Clean(requestedResolved), nil
}

func (b *journalActionBoundary) canonicalPaths(
	cwd string,
	requirement ApprovalRequirement,
	operation string,
) ([]string, error) {
	paths := slices.Clone(requirement.Paths)
	if len(paths) == 0 && (strings.EqualFold(operation, "write") || strings.EqualFold(operation, "edit")) &&
		requirement.Resource != "" {
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
	slices.Sort(canonical)
	return slices.Compact(canonical), nil
}

func canonicalStringList(values []string) []string {
	canonical := make([]string, 0, len(values))
	for _, value := range values {
		value = strings.TrimSpace(value)
		if value != "" {
			canonical = append(canonical, value)
		}
	}
	sort.Strings(canonical)
	return slices.Compact(canonical)
}

func pathWithin(root, candidate string) bool {
	rel, err := filepath.Rel(root, candidate)
	return err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
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
	if !pathWithin(root, path) {
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
