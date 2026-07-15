package export

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strconv"
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

// DecodeSessionBundle accepts Ion's JSON bundle and Pi-style JSONL session
// files. JSONL imports always become a fresh Ion session, avoiding ID clashes.
func DecodeSessionBundle(data []byte) (SessionBundle, error) {
	var bundle SessionBundle
	if err := json.Unmarshal(data, &bundle); err == nil && len(bundle.Sessions) > 0 {
		return bundle, nil
	}
	return decodeJSONL(data)
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
		return remap[id]
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

func decodeJSONL(data []byte) (SessionBundle, error) {
	lines := strings.Split(string(data), "\n")
	var info SessionBundleInfo
	var events []json.RawMessage
	var sawLine bool
	for lineNumber, line := range lines {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		sawLine = true
		var raw map[string]json.RawMessage
		if err := json.Unmarshal([]byte(line), &raw); err != nil {
			return SessionBundle{}, fmt.Errorf("decode JSONL line %d: %w", lineNumber+1, err)
		}
		typ := stringField(raw, "type")
		if typ == "session" {
			info.ID = stringField(raw, "id")
			info.Workdir = stringField(raw, "cwd")
			info.UpdatedAt = timeField(raw["timestamp"])
			if info.UpdatedAt.IsZero() {
				info.UpdatedAt = time.Now()
			}
			continue
		}
		converted, err := convertJSONLLine(raw)
		if err != nil {
			return SessionBundle{}, fmt.Errorf("convert JSONL line %d: %w", lineNumber+1, err)
		}
		events = append(events, converted)
	}
	if !sawLine || len(events) == 0 {
		return SessionBundle{}, errors.New("input is neither a session bundle nor a non-empty JSONL session")
	}
	for _, raw := range events {
		entry, err := session.UnmarshalEntry(raw)
		if err != nil {
			return SessionBundle{}, fmt.Errorf("decode converted JSONL entry: %w", err)
		}
		if model, ok := entry.(*session.ModelChangeEntry); ok {
			info.Model = model.Provider + "/" + model.ModelID
		}
		if named, ok := entry.(*session.SessionInfoEntry); ok && strings.TrimSpace(named.Name) != "" {
			info.Name = named.Name
		}
	}
	if info.UpdatedAt.IsZero() {
		info.UpdatedAt = time.Now()
	}
	return SessionBundle{
		Version:    sessionBundleVersion,
		Sessions:   []SessionBundleRecord{{Info: info, Events: events}},
		ExportedAt: time.Now(),
	}, nil
}

func convertJSONLLine(raw map[string]json.RawMessage) (json.RawMessage, error) {
	if _, ok := raw["payload"]; ok && stringField(raw, "id") != "" {
		encoded, err := json.Marshal(raw)
		return encoded, err
	}
	typ := stringField(raw, "type")
	id := stringField(raw, "id")
	if typ == "" || id == "" {
		return nil, errors.New("entry requires type and id")
	}
	payload := make(map[string]json.RawMessage)
	copyField := func(destination, source string) {
		if value, ok := raw[source]; ok {
			payload[destination] = value
		}
	}
	switch typ {
	case "message":
		copyField("message", "message")
	case "model_change":
		copyField("provider", "provider")
		copyField("model_id", "modelId")
	case "thinking_level_change":
		typ = "thinking_change"
		copyField("level", "thinkingLevel")
	case "active_tools_change":
		typ = "tools_change"
		copyField("active_tools", "activeToolNames")
	case "compaction":
		copyField("summary", "summary")
		copyField("first_kept_id", "firstKeptEntryId")
		copyField("tokens_before", "tokensBefore")
		copyJSONBytesField(payload, "details", raw, "details")
	case "branch_summary":
		copyField("from_id", "fromId")
		copyField("summary", "summary")
		copyJSONBytesField(payload, "details", raw, "details")
	case "label":
		copyField("target_id", "targetId")
		copyField("label", "label")
	case "session_info":
		copyField("name", "name")
	case "custom":
		copyField("custom_type", "customType")
		copyField("custom_data", "data")
	case "leaf":
		copyField("leaf_target_id", "targetId")
	case "custom_message":
		copyField("custom_message_type", "customType")
		copyField("custom_message_content", "content")
		copyField("custom_message_display", "display")
		copyJSONBytesField(payload, "custom_message_details", raw, "details")
	default:
		return nil, fmt.Errorf("unsupported entry type %q", typ)
	}
	payloadJSON, err := json.Marshal(payload)
	if err != nil {
		return nil, err
	}
	envelope := map[string]any{
		"id":        id,
		"parent_id": stringField(raw, "parentId"),
		"type":      typ,
		"timestamp": timeField(raw["timestamp"]).UnixMilli(),
		"payload":   json.RawMessage(payloadJSON),
	}
	return json.Marshal(envelope)
}

func copyJSONBytesField(destinationPayload map[string]json.RawMessage, destination string, raw map[string]json.RawMessage, source string) {
	value, ok := raw[source]
	if !ok || string(value) == "null" {
		return
	}
	encoded := base64.StdEncoding.EncodeToString(value)
	destinationPayload[destination], _ = json.Marshal(encoded)
}

func stringField(raw map[string]json.RawMessage, name string) string {
	value, ok := raw[name]
	if !ok || string(value) == "null" {
		return ""
	}
	var result string
	_ = json.Unmarshal(value, &result)
	return result
}

func timeField(raw json.RawMessage) time.Time {
	if len(raw) == 0 || string(raw) == "null" {
		return time.Time{}
	}
	var text string
	if json.Unmarshal(raw, &text) == nil {
		if parsed, err := time.Parse(time.RFC3339Nano, text); err == nil {
			return parsed
		}
	}
	number, err := strconv.ParseInt(strings.Trim(string(raw), `"`), 10, 64)
	if err == nil {
		return time.UnixMilli(number)
	}
	return time.Time{}
}
