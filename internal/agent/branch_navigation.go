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
	stream := h.stream
	auth := h.auth
	reserveTokens := h.compaction.ReserveTokens
	runCancel := h.runCancel
	h.mu.Unlock()

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
			opts.CustomInstructions,
		)
		if err != nil {
			if errors.Is(err, context.Canceled) || navigationCanceled(navigationCtx, runCancel) {
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
	customInstructions string,
) (*session.BranchSummaryData, error) {
	messages := branchSummaryMessages(entries)
	if len(messages) == 0 {
		return &session.BranchSummaryData{Summary: branchSummaryPreamble + "No content to summarize"}, nil
	}
	if stream == nil {
		return nil, errors.New("no model stream configured")
	}
	if reserveTokens <= 0 {
		reserveTokens = 16384
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
	)
	if err != nil {
		return nil, err
	}

	fileOps := ExtractFileOperations(messages, entries, -1)
	readFiles, modifiedFiles := ComputeFileLists(fileOps)
	normalizedFileOps := &CompactionFileOps{ReadFiles: readFiles, ModifiedFiles: modifiedFiles}
	details, err := json.Marshal(CompactionFileOps{ReadFiles: readFiles, ModifiedFiles: modifiedFiles})
	if err != nil {
		return nil, fmt.Errorf("marshal branch details: %w", err)
	}
	return &session.BranchSummaryData{
		Summary: branchSummaryPreamble + summaryResult.Text + FormatFileOperations(normalizedFileOps),
		Usage:   summaryResult.Usage,
		Details: details,
	}, nil
}

func branchSummaryMessages(entries []session.Entry) []session.Message {
	messages := make([]session.Message, 0, len(entries))
	for _, entry := range entries {
		switch entry := entry.(type) {
		case *session.MessageEntry:
			messages = append(messages, entry.Message)
		case *session.CustomMessageEntry:
			messages = append(messages, &session.CustomMessage{
				CustomType: entry.CustomType,
				Content:    entry.Content,
				Display:    entry.Display,
				Details:    entry.Details,
				Timestamp:  entry.Timestamp,
			})
		case *session.CompactionEntry:
			messages = append(messages, session.NewUserText(
				session.CompactionSummaryPrefix+entry.Summary+session.CompactionSummarySuffix,
				entry.Timestamp,
			))
		case *session.BranchSummaryEntry:
			messages = append(messages, session.NewUserText(
				session.BranchSummaryPrefix+entry.Summary+session.BranchSummarySuffix,
				entry.Timestamp,
			))
		}
	}
	return messages
}
