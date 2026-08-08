package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

const branchSummaryPreamble = `The user explored a different conversation branch before returning here.
Summary of that exploration:

`

// NavigateTree moves the session leaf and optionally summarizes the abandoned
// branch. The handler reserves PhaseRecovering before starting this worker;
// Session owns validation, leaf persistence, and the branch_summary entry.
func (h *Controller) navigateTreeDirect(
	ctx context.Context,
	targetID string,
	opts NavigateOptions,
) (result NavigateResult, err error) {
	h.mu.Lock()
	model := h.model
	thinking := h.thinking
	auth := h.auth
	summaryRetry := h.summaryRetry
	reserveTokens := h.compaction.ReserveTokens
	contextWindow := h.contextWindow
	runCancel := h.runCancel
	h.mu.Unlock()
	stream := h.wrapStreamFn()

	if h.session == nil {
		return result, errors.New("harness has no session")
	}
	if runCancel == nil {
		return result, errors.New("branch navigation is not reserved")
	}
	navigationCtx, releaseNavigationCtx := h.runtimeRunBoundContext(ctx, runCancel)
	defer releaseNavigationCtx()

	oldLeafID := h.session.GetLeafID()
	if oldLeafID == targetID {
		result.LeafID = oldLeafID
		return result, nil
	}
	if _, err := h.session.GetEntry(navigationCtx, targetID); err != nil {
		return result, fmt.Errorf("navigate tree: target entry %q not found: %w", targetID, err)
	}

	entries, err := h.collectBranchEntries(navigationCtx, oldLeafID, targetID)
	if err != nil {
		return result, fmt.Errorf("navigate tree: collect branch: %w", err)
	}
	targetBranch, err := h.session.BranchAt(navigationCtx, targetID)
	if err != nil {
		return result, fmt.Errorf("navigate tree: read target branch: %w", err)
	}
	targetContext, err := session.ProjectContext(targetBranch)
	if err != nil {
		return result, fmt.Errorf("navigate tree: restore target runtime state: %w", err)
	}

	var summary *session.BranchSummaryData
	if opts.Summarize {
		summary, err = h.summarizeBranch(
			navigationCtx,
			runCancel,
			entries,
			model,
			thinking,
			stream,
			auth,
			reserveTokens,
			contextWindow,
			opts.CustomInstructions,
			summaryRetry,
		)
		if err != nil {
			if errors.Is(err, context.Canceled) || navigationCanceled(navigationCtx, runCancel) {
				if llm.IsStreamCleanupError(err) {
					return result, errors.Join(context.Canceled, err)
				}
				return result, context.Canceled
			}
			return result, fmt.Errorf("navigate tree: summarize branch: %w", err)
		}
	}
	if navigationCanceled(navigationCtx, runCancel) {
		return result, context.Canceled
	}

	if summary != nil {
		summary.FromID = oldLeafID
	}
	result.SummaryEntryID, err = h.session.MoveTo(navigationCtx, targetID, summary)
	if err != nil {
		return result, fmt.Errorf("navigate tree: move session: %w", err)
	}
	result.LeafID = h.session.GetLeafID()
	result.ActiveProvider = targetContext.ActiveProvider
	result.ActiveModel = targetContext.ActiveModel
	return result, nil
}

func navigationCanceled(ctx context.Context, cancel <-chan struct{}) bool {
	select {
	case <-ctx.Done():
		return true
	case <-cancel:
		return true
	default:
		return false
	}
}

// collectBranchEntries returns the old branch segment after the common
// ancestor, in chronological order. This mirrors Pi's
// collectEntriesForBranchSummary and deliberately runs before MoveTo changes
// the session leaf.
func (h *Controller) collectBranchEntries(ctx context.Context, oldLeafID, targetID string) ([]session.Entry, error) {
	if oldLeafID == "" {
		return nil, nil
	}
	oldBranch, err := h.session.Branch(ctx)
	if err != nil {
		return nil, err
	}
	oldPath := make(map[string]struct{}, len(oldBranch))
	for _, entry := range oldBranch {
		oldPath[entry.ID()] = struct{}{}
	}

	commonAncestorID := ""
	for currentID := targetID; currentID != ""; {
		entry, err := h.session.GetEntry(ctx, currentID)
		if err != nil {
			return nil, err
		}
		if _, ok := oldPath[entry.ID()]; ok {
			commonAncestorID = entry.ID()
			break
		}
		currentID = entry.ParentID()
	}

	var entries []session.Entry
	for currentID := oldLeafID; currentID != "" && currentID != commonAncestorID; {
		entry, err := h.session.GetEntry(ctx, currentID)
		if err != nil {
			return nil, err
		}
		entries = append(entries, entry)
		currentID = entry.ParentID()
	}
	for left, right := 0, len(entries)-1; left < right; left, right = left+1, right-1 {
		entries[left], entries[right] = entries[right], entries[left]
	}
	return entries, nil
}

func (h *Controller) summarizeBranch(
	ctx context.Context,
	runCancel <-chan struct{},
	entries []session.Entry,
	model llm.Model,
	thinking session.ThinkingLevel,
	stream func(context.Context, *llm.Request) (llm.Stream, error),
	auth func(llm.Model) (string, map[string]string),
	reserveTokens int,
	contextWindow int,
	customInstructions string,
	summaryRetry llm.StreamRetryPolicy,
) (*session.BranchSummaryData, error) {
	if contextWindow <= 0 {
		contextWindow = model.ContextWindow
	}
	reserveTokens = normalizeCompactionSettings(
		CompactionSettings{ReserveTokens: reserveTokens}, contextWindow,
	).ReserveTokens
	// Bound the summarization prompt to the model's available context while
	// keeping the newest abandoned-branch work. Unknown windows remain
	// unbounded here; the provider is still the final overflow authority.
	tokenBudget := -1 // unknown window: unbounded preparation
	if contextWindow > 0 {
		tokenBudget = contextWindow - reserveTokens
		if tokenBudget < 0 {
			tokenBudget = 0
		}
	}
	fileOps := branchSummaryFileOperations(entries)
	messages := branchSummaryMessages(entries, tokenBudget)
	if len(messages) == 0 {
		details, err := marshalBranchSummaryDetails(fileOps)
		if err != nil {
			return nil, err
		}
		return &session.BranchSummaryData{
			Summary: branchSummaryPreamble + "No content to summarize" + FormatFileOperations(fileOps),
			Details: details,
		}, nil
	}
	if stream == nil {
		return nil, errors.New("no model stream configured")
	}

	apiKey := ""
	var headers map[string]string
	if auth != nil {
		apiKey, headers = auth(model)
	}
	streamCtx, cancel := context.WithCancel(ctx)
	stop := make(chan struct{})
	go func() {
		select {
		case <-runCancel:
			cancel()
		case <-stop:
		case <-streamCtx.Done():
		}
	}()
	defer func() {
		close(stop)
		cancel()
	}()

	summaryResult, err := GenerateSummary(
		streamCtx,
		messages,
		model.ID,
		reserveTokens,
		model.MaxTokens,
		apiKey,
		headers,
		runCancel,
		customInstructions,
		"",
		thinking,
		DefaultConvert,
		stream,
		summaryRetry,
	)
	if err != nil {
		return nil, err
	}

	for _, message := range messages {
		ExtractFileOperationsFromMessage(message, fileOps)
	}
	details, err := marshalBranchSummaryDetails(fileOps)
	if err != nil {
		return nil, err
	}
	readFiles, modifiedFiles := ComputeFileLists(fileOps)
	normalizedFileOps := &CompactionFileOps{ReadFiles: readFiles, ModifiedFiles: modifiedFiles}
	return &session.BranchSummaryData{
		Summary: branchSummaryPreamble + summaryResult.Text + FormatFileOperations(normalizedFileOps),
		Usage:   summaryResult.Usage,
		Details: details,
	}, nil
}

func branchSummaryMessages(entries []session.Entry, tokenBudget int) []session.Message {
	// Walk newest-first so a large abandoned branch retains its most recent
	// work. Tool results are omitted from the model prompt; file-operation
	// extraction still sees the original entries below.
	selected := make([]session.Message, 0, len(entries))
	totalTokens := 0
	for index := len(entries) - 1; index >= 0; index-- {
		message := branchSummaryMessage(entries[index])
		if message == nil {
			continue
		}
		tokens := EstimateTokens(message)
		if tokenBudget > 0 && totalTokens+tokens > tokenBudget {
			// Summary markers are important context. Include one that slightly
			// exceeds the budget when the selected context is still sparse, as
			// Pi does, then stop at the boundary.
			_, isCompaction := entries[index].(*session.CompactionEntry)
			_, isBranchSummary := entries[index].(*session.BranchSummaryEntry)
			if (!isCompaction && !isBranchSummary) || totalTokens >= tokenBudget*9/10 {
				break
			}
		}
		selected = append(selected, message)
		totalTokens += tokens
	}
	for left, right := 0, len(selected)-1; left < right; left, right = left+1, right-1 {
		selected[left], selected[right] = selected[right], selected[left]
	}
	return selected
}

func branchSummaryMessage(entry session.Entry) session.Message {
	switch entry := entry.(type) {
	case *session.MessageEntry:
		if _, ok := entry.Message.(*session.ToolResultMessage); ok {
			return nil
		}
		return entry.Message
	case *session.CustomMessageEntry:
		return &session.CustomMessage{
			CustomType: entry.CustomType,
			Content:    entry.Content,
			Display:    entry.Display,
			Details:    entry.Details,
			Timestamp:  entry.Timestamp,
		}
	case *session.CompactionEntry:
		return session.NewUserText(
			session.CompactionSummaryPrefix+entry.Summary+session.CompactionSummarySuffix,
			entry.Timestamp,
		)
	case *session.BranchSummaryEntry:
		return session.NewUserText(
			session.BranchSummaryPrefix+entry.Summary+session.BranchSummarySuffix,
			entry.Timestamp,
		)
	default:
		return nil
	}
}

func branchSummaryFileOperations(entries []session.Entry) *CompactionFileOps {
	fileOps := &CompactionFileOps{}
	for _, entry := range entries {
		if messageEntry, ok := entry.(*session.MessageEntry); ok {
			ExtractFileOperationsFromMessage(messageEntry.Message, fileOps)
			continue
		}
		var rawDetails []byte
		switch typed := entry.(type) {
		case *session.BranchSummaryEntry:
			rawDetails = typed.Details
		case *session.CompactionEntry:
			rawDetails = typed.Details
		default:
			continue
		}
		if len(rawDetails) == 0 {
			continue
		}
		var details CompactionFileOps
		if err := json.Unmarshal(rawDetails, &details); err != nil {
			continue
		}
		for _, path := range details.ReadFiles {
			addIfNew(fileOps, "read", path)
		}
		for _, path := range details.ModifiedFiles {
			addIfNew(fileOps, "modified", path)
		}
	}
	return fileOps
}

func marshalBranchSummaryDetails(fileOps *CompactionFileOps) ([]byte, error) {
	readFiles, modifiedFiles := ComputeFileLists(fileOps)
	details, err := json.Marshal(CompactionFileOps{ReadFiles: readFiles, ModifiedFiles: modifiedFiles})
	if err != nil {
		return nil, fmt.Errorf("marshal branch details: %w", err)
	}
	return details, nil
}
