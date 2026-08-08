package export

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/nijaru/ion/session"
)

const sessionBundleVersion = 1

// SessionBundle is Ion's explicit transport format. Entries are encoded as
// session envelopes so their tree identity survives a file boundary.
type SessionBundle struct {
	Version       int                   `json:"version"`
	RootSessionID string                `json:"root_session_id,omitempty"`
	Sessions      []SessionBundleRecord `json:"sessions"`
	ExportedAt    time.Time             `json:"exported_at"`
}

type SessionBundleRecord struct {
	Info   SessionBundleInfo `json:"info"`
	Events []json.RawMessage `json:"events"`
}

type SessionBundleInfo struct {
	ID          string    `json:"id"`
	Workdir     string    `json:"workdir,omitempty"`
	Model       string    `json:"model,omitempty"`
	Branch      string    `json:"branch,omitempty"`
	Name        string    `json:"name,omitempty"`
	Summary     string    `json:"summary,omitempty"`
	LastPreview string    `json:"last_preview,omitempty"`
	UpdatedAt   time.Time `json:"updated_at"`
}

type catalogFinder interface {
	GetSessionInfo(context.Context, string) (session.SessionInfoEntry, error)
}

type catalogWriter interface {
	UpdateSession(context.Context, session.SessionInfoEntry) error
}

// ExportSessionBundle exports the branch ending at leafID. It does not move
// the active leaf, so exporting a selected/resumed session is read-only.
func ExportSessionBundle(ctx context.Context, store session.Store, leafID string) (SessionBundle, error) {
	if store == nil {
		return SessionBundle{}, errors.New("session store is required")
	}
	leafID = strings.TrimSpace(leafID)
	if leafID == "" {
		return SessionBundle{}, errors.New("session leaf ID is required")
	}
	entries, err := branchAt(ctx, store, leafID)
	if err != nil {
		return SessionBundle{}, fmt.Errorf("export session %s: %w", leafID, err)
	}

	info := SessionBundleInfo{ID: leafID, UpdatedAt: time.Now()}
	if finder, ok := store.(catalogFinder); ok {
		catalog, err := finder.GetSessionInfo(ctx, leafID)
		if err == nil {
			info = sessionBundleInfo(catalog)
			info.ID = leafID
		} else if !errors.Is(err, sql.ErrNoRows) {
			return SessionBundle{}, fmt.Errorf("read session %s metadata: %w", leafID, err)
		}
	}
	meta := store.Meta()
	if info.Workdir == "" {
		info.Workdir = meta.CWD
	}
	if info.Branch == "" {
		info.Branch = meta.Branch
	}
	if info.Name == "" {
		info.Name = meta.Name
	}

	events := make([]json.RawMessage, 0, len(entries))
	for _, entry := range entries {
		encoded, err := session.MarshalEntry(entry)
		if err != nil {
			return SessionBundle{}, fmt.Errorf("marshal entry %s: %w", entry.ID(), err)
		}
		events = append(events, encoded)
	}
	return SessionBundle{
		Version:       sessionBundleVersion,
		RootSessionID: leafID,
		Sessions:      []SessionBundleRecord{{Info: info, Events: events}},
		ExportedAt:    time.Now(),
	}, nil
}

// ForkSession copies one branch into a fresh disconnected tree in the same
// store and makes the new leaf active. The source entries and source leaf are
// never modified.
func ForkSession(ctx context.Context, store session.Store, leafID string) (string, error) {
	bundle, err := ExportSessionBundle(ctx, store, leafID)
	if err != nil {
		return "", err
	}
	bundle.RootSessionID = ""
	return ImportSessionBundle(ctx, store, bundle)
}

// ImportSessionBundle imports one session record and returns its new leaf ID.
// A blank RootSessionID means remap all entry IDs, which is the safe default
// for user imports and forks. A non-blank root permits an exact import only
// when none of the source IDs already exist in the destination store.
func ImportSessionBundle(ctx context.Context, store session.Store, bundle SessionBundle) (string, error) {
	if store == nil {
		return "", errors.New("session store is required")
	}
	if bundle.Version > sessionBundleVersion {
		return "", fmt.Errorf("unsupported session bundle version %d", bundle.Version)
	}
	if len(bundle.Sessions) != 1 {
		return "", fmt.Errorf("session bundle must contain exactly one session, got %d", len(bundle.Sessions))
	}
	record := bundle.Sessions[0]
	if len(record.Events) == 0 {
		return "", errors.New("session bundle contains no entries")
	}

	sourceEntries := make([]session.Entry, 0, len(record.Events))
	byID := make(map[string]session.Entry, len(record.Events))
	for i, raw := range record.Events {
		entry, err := session.UnmarshalEntry(raw)
		if err != nil {
			return "", fmt.Errorf("decode imported entry %d: %w", i+1, err)
		}
		if entry.ID() == "" {
			return "", fmt.Errorf("imported entry %d has empty ID", i+1)
		}
		if _, exists := byID[entry.ID()]; exists {
			return "", fmt.Errorf("duplicate imported entry ID %q", entry.ID())
		}
		byID[entry.ID()] = entry
		sourceEntries = append(sourceEntries, entry)
	}

	sourceLeaf := strings.TrimSpace(bundle.RootSessionID)
	if sourceLeaf == "" {
		candidate := strings.TrimSpace(record.Info.ID)
		if _, ok := byID[candidate]; ok {
			sourceLeaf = candidate
		}
	}
	if sourceLeaf == "" {
		sourceLeaf = sourceEntries[len(sourceEntries)-1].ID()
	}
	if _, ok := byID[sourceLeaf]; !ok {
		return "", fmt.Errorf("session bundle leaf %q is not present in entries", sourceLeaf)
	}
	if err := validateImportedEntries(byID); err != nil {
		return "", fmt.Errorf("validate imported session: %w", err)
	}
	if err := validateImportedCompactions(byID); err != nil {
		return "", fmt.Errorf("validate imported session: %w", err)
	}

	preserveIDs := strings.TrimSpace(bundle.RootSessionID) != ""
	remap := make(map[string]string, len(sourceEntries))
	if preserveIDs {
		for _, entry := range sourceEntries {
			exists, err := entryExists(ctx, store, entry.ID())
			if err != nil {
				return "", fmt.Errorf("check imported entry %s: %w", entry.ID(), err)
			}
			if exists {
				return "", fmt.Errorf("session entry %q already exists", entry.ID())
			}
			remap[entry.ID()] = entry.ID()
		}
	} else {
		used := make(map[string]struct{}, len(sourceEntries))
		for _, entry := range sourceEntries {
			id := freshEntryID(used)
			used[id] = struct{}{}
			remap[entry.ID()] = id
		}
	}

	imported := make([]session.Entry, 0, len(sourceEntries))
	for _, source := range sourceEntries {
		parentID := remap[source.ParentID()]
		entry, err := cloneEntry(source, session.EntryBase{
			ID:        remap[source.ID()],
			ParentID:  parentID,
			Timestamp: source.When(),
		}, remap)
		if err != nil {
			return "", fmt.Errorf("clone imported entry %s: %w", source.ID(), err)
		}
		imported = append(imported, entry)
	}

	newLeafID := remap[sourceLeaf]
	if _, err := store.AppendBatch(ctx, imported); err != nil {
		return "", fmt.Errorf("persist imported session: %w", err)
	}
	if err := store.SetLeafID(newLeafID); err != nil {
		return "", fmt.Errorf("activate imported session: %w", err)
	}

	info := record.Info
	info.ID = newLeafID
	info.UpdatedAt = time.Now()
	if writer, ok := store.(catalogWriter); ok {
		if err := writer.UpdateSession(ctx, sessionInfoEntry(info)); err != nil {
			return "", fmt.Errorf("persist imported session metadata: %w", err)
		}
	}
	return newLeafID, nil
}

func validateImportedEntries(byID map[string]session.Entry) error {
	const (
		visiting = 1
		complete = 2
	)
	states := make(map[string]uint8, len(byID))
	for entryID := range byID {
		if states[entryID] == complete {
			continue
		}
		var path []string
		currentID := entryID
		for {
			switch states[currentID] {
			case visiting:
				return fmt.Errorf("entry parent cycle at %q", currentID)
			case complete:
				currentID = ""
			}
			if currentID == "" {
				break
			}
			current, ok := byID[currentID]
			if !ok {
				return fmt.Errorf("entry %q is not present in imported entries", currentID)
			}
			states[currentID] = visiting
			path = append(path, currentID)
			if current.ParentID() == "" {
				break
			}
			if _, ok := byID[current.ParentID()]; !ok {
				return fmt.Errorf("entry %q parent %q not found", current.ID(), current.ParentID())
			}
			currentID = current.ParentID()
		}
		for _, id := range path {
			states[id] = complete
		}
	}
	return nil
}

func validateImportedCompactions(byID map[string]session.Entry) error {
	for _, entry := range byID {
		compaction, ok := entry.(*session.CompactionEntry)
		if !ok {
			continue
		}
		if compaction.FirstKeptID == "" {
			return fmt.Errorf("compaction %q has no first-kept entry", compaction.ID())
		}
		found := false
		for currentID := compaction.ParentID(); currentID != ""; {
			if currentID == compaction.FirstKeptID {
				found = true
				break
			}
			parent, ok := byID[currentID]
			if !ok {
				return fmt.Errorf("compaction %q parent %q not found", compaction.ID(), currentID)
			}
			currentID = parent.ParentID()
		}
		if !found {
			return fmt.Errorf(
				"compaction %q first-kept entry %q is not an ancestor",
				compaction.ID(),
				compaction.FirstKeptID,
			)
		}
	}
	return nil
}

func branchAt(ctx context.Context, store session.Store, leafID string) ([]session.Entry, error) {
	entries, err := store.Entries(ctx)
	if err != nil {
		return nil, err
	}
	byID := make(map[string]session.Entry, len(entries))
	for _, entry := range entries {
		if entry != nil {
			byID[entry.ID()] = entry
		}
	}
	current, ok := byID[leafID]
	if !ok {
		return nil, fmt.Errorf("leaf %q not found", leafID)
	}
	var reverse []session.Entry
	seen := map[string]bool{}
	for current != nil {
		if seen[current.ID()] {
			return nil, fmt.Errorf("entry parent cycle at %q", current.ID())
		}
		seen[current.ID()] = true
		reverse = append(reverse, current)
		if current.ParentID() == "" {
			break
		}
		parent, ok := byID[current.ParentID()]
		if !ok {
			return nil, fmt.Errorf("entry %q parent %q not found", current.ID(), current.ParentID())
		}
		current = parent
	}
	for i, j := 0, len(reverse)-1; i < j; i, j = i+1, j-1 {
		reverse[i], reverse[j] = reverse[j], reverse[i]
	}
	return reverse, nil
}

func entryExists(ctx context.Context, store session.Store, id string) (bool, error) {
	_, err := store.GetEntry(ctx, id)
	if err == nil {
		return true, nil
	}
	if errors.Is(err, sql.ErrNoRows) || os.IsNotExist(err) {
		return false, nil
	}
	return false, err
}

func freshEntryID(used map[string]struct{}) string {
	for {
		var raw [16]byte
		if _, err := rand.Read(raw[:]); err != nil {
			panic(fmt.Sprintf("generate session entry ID: %v", err))
		}
		id := hex.EncodeToString(raw[:])
		if _, ok := used[id]; !ok {
			return id
		}
	}
}

func cloneEntry(source session.Entry, base session.EntryBase, remap map[string]string) (session.Entry, error) {
	remapRef := func(id string) string {
		if id == "" {
			return ""
		}
		if mapped, ok := remap[id]; ok {
			return mapped
		}
		// Branch-summary provenance may point at an abandoned branch that is
		// intentionally outside this exported branch. Preserve that opaque
		// reference instead of silently erasing it.
		return id
	}
	switch entry := source.(type) {
	case *session.MessageEntry:
		return &session.MessageEntry{EntryBase: base, Message: cloneMessage(entry.Message)}, nil
	case *session.ModelChangeEntry:
		copy := *entry
		copy.EntryBase = base
		return &copy, nil
	case *session.ThinkingChangeEntry:
		copy := *entry
		copy.EntryBase = base
		return &copy, nil
	case *session.ToolsChangeEntry:
		copy := *entry
		copy.EntryBase = base
		copy.ActiveTools = append([]string(nil), entry.ActiveTools...)
		return &copy, nil
	case *session.CompactionEntry:
		copy := *entry
		copy.EntryBase = base
		copy.FirstKeptID = remapRef(entry.FirstKeptID)
		copy.Details = append([]byte(nil), entry.Details...)
		return &copy, nil
	case *session.BranchSummaryEntry:
		copy := *entry
		copy.EntryBase = base
		copy.FromID = remapRef(entry.FromID)
		copy.Details = append([]byte(nil), entry.Details...)
		return &copy, nil
	case *session.LabelEntry:
		copy := *entry
		copy.EntryBase = base
		copy.TargetID = remapRef(entry.TargetID)
		return &copy, nil
	case *session.SessionInfoEntry:
		copy := *entry
		copy.EntryBase = base
		return &copy, nil
	case *session.CustomEntry:
		copy := *entry
		copy.EntryBase = base
		copy.Data = append([]byte(nil), entry.Data...)
		return &copy, nil
	case *session.LeafEntry:
		copy := *entry
		copy.EntryBase = base
		copy.TargetID = remapRef(entry.TargetID)
		return &copy, nil
	case *session.CustomMessageEntry:
		copy := *entry
		copy.EntryBase = base
		copy.Content = cloneContent(entry.Content)
		copy.Details = append([]byte(nil), entry.Details...)
		return &copy, nil
	default:
		return nil, fmt.Errorf("unsupported entry type %T", source)
	}
}

func cloneMessage(message session.Message) session.Message {
	switch message := message.(type) {
	case *session.UserMessage:
		copy := *message
		copy.Content = cloneContent(message.Content)
		return &copy
	case *session.AssistantMessage:
		copy := *message
		copy.Content = cloneContent(message.Content)
		return &copy
	case *session.ToolResultMessage:
		copy := *message
		copy.Content = cloneContent(message.Content)
		copy.Details = append([]byte(nil), message.Details...)
		return &copy
	case *session.CustomMessage:
		copy := *message
		copy.Content = cloneContent(message.Content)
		copy.Details = append([]byte(nil), message.Details...)
		return &copy
	default:
		return message
	}
}

func cloneContent(content []session.Content) []session.Content {
	copyContent := make([]session.Content, len(content))
	for i, block := range content {
		switch block := block.(type) {
		case session.TextContent:
			copyContent[i] = block
		case session.ThinkingContent:
			copyContent[i] = block
		case session.ImageContent:
			block.Data = append([]byte(nil), block.Data...)
			copyContent[i] = block
		case *session.ToolCall:
			copy := *block
			copy.Arguments = make(map[string]any, len(block.Arguments))
			for key, value := range block.Arguments {
				copy.Arguments[key] = value
			}
			copyContent[i] = &copy
		default:
			copyContent[i] = block
		}
	}
	return copyContent
}

func sessionBundleInfo(info session.SessionInfoEntry) SessionBundleInfo {
	return SessionBundleInfo{
		ID:          info.ID(),
		Workdir:     info.Workdir,
		Model:       info.Model,
		Branch:      info.Branch,
		Name:        info.Name,
		Summary:     info.Summary,
		LastPreview: info.LastPreview,
		UpdatedAt:   info.UpdatedAt,
	}
}

func sessionInfoEntry(info SessionBundleInfo) session.SessionInfoEntry {
	return session.SessionInfoEntry{
		EntryBase:   session.EntryBase{ID: info.ID, Timestamp: info.UpdatedAt},
		Workdir:     info.Workdir,
		Model:       info.Model,
		Branch:      info.Branch,
		Name:        info.Name,
		Summary:     info.Summary,
		LastPreview: info.LastPreview,
		UpdatedAt:   info.UpdatedAt,
	}
}
