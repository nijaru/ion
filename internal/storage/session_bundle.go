package storage

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	csession "github.com/nijaru/canto/session"
)

const sessionBundleVersion = 1

var (
	ErrSessionBundleConflict  = errors.New("session bundle conflict")
	ErrSessionBundleIntegrity = errors.New("session bundle integrity")
)

type preparedBundleRecord struct {
	record   SessionBundleRecord
	ancestry csession.SessionAncestry
	events   []csession.Event
}

func (s *cantoStore) ExportSessionBundle(
	ctx context.Context,
	sessionID string,
) (SessionBundle, error) {
	records, err := s.exportBundleLineage(ctx, sessionID)
	if err != nil {
		return SessionBundle{}, err
	}

	bundle := SessionBundle{
		Version:       sessionBundleVersion,
		ExportedAt:    time.Now().UTC(),
		RootSessionID: sessionID,
		Sessions:      make([]SessionBundleRecord, 0, len(records)),
	}
	for _, ancestry := range records {
		info, err := s.sessionInfo(ctx, ancestry.SessionID)
		if err != nil {
			return SessionBundle{}, fmt.Errorf("load session info %s: %w", ancestry.SessionID, err)
		}
		sess, err := s.canto.Load(ctx, ancestry.SessionID)
		if err != nil {
			return SessionBundle{}, fmt.Errorf("load canto session %s: %w", ancestry.SessionID, err)
		}
		events := sess.Events()
		rawEvents := make([]json.RawMessage, 0, len(events))
		for _, event := range events {
			raw, err := csession.MarshalEventJSON(event)
			if err != nil {
				return SessionBundle{}, fmt.Errorf("encode event %s: %w", event.ID, err)
			}
			rawEvents = append(rawEvents, json.RawMessage(raw))
		}
		bundle.Sessions = append(bundle.Sessions, SessionBundleRecord{
			Info:          info,
			Ancestry:      ancestry,
			Events:        rawEvents,
			EventCount:    len(rawEvents),
			EventChecksum: checksumBundleEvents(rawEvents),
		})
	}
	if err := bundle.seal(); err != nil {
		return SessionBundle{}, err
	}
	return bundle, nil
}

func (s *cantoStore) ImportSessionBundle(
	ctx context.Context,
	bundle SessionBundle,
) ([]SessionInfo, error) {
	prepared, err := prepareSessionBundle(bundle)
	if err != nil {
		return nil, err
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	for _, item := range prepared {
		if exists, err := s.sessionBundleMetaExists(ctx, item.record.Info.ID); err != nil {
			return nil, err
		} else if exists {
			return nil, fmt.Errorf("%w: session %s already exists in ion index",
				ErrSessionBundleConflict, item.record.Info.ID)
		}
		if exists, err := s.cantoBundleSessionExists(ctx, item.record.Info.ID); err != nil {
			return nil, err
		} else if exists {
			return nil, fmt.Errorf("%w: session %s already exists in canto store",
				ErrSessionBundleConflict, item.record.Info.ID)
		}
	}

	imported := make([]SessionInfo, 0, len(prepared))
	for _, item := range prepared {
		if err := s.canto.SaveAncestry(ctx, item.ancestry); err != nil {
			return nil, fmt.Errorf("import ancestry %s: %w", item.ancestry.SessionID, err)
		}
		if err := s.insertBundleSessionMeta(ctx, item.record.Info); err != nil {
			return nil, fmt.Errorf("import session meta %s: %w", item.record.Info.ID, err)
		}
		for _, event := range item.events {
			if err := s.canto.Save(ctx, event); err != nil {
				return nil, fmt.Errorf("import event %s: %w", event.ID, err)
			}
		}
		imported = append(imported, item.record.Info)
	}
	return imported, nil
}

func (s *cantoStore) exportBundleLineage(
	ctx context.Context,
	sessionID string,
) ([]SessionAncestryInfo, error) {
	info, err := s.sessionInfo(ctx, sessionID)
	if err != nil {
		return nil, err
	}
	lineage, err := s.canto.Lineage(ctx, sessionID)
	if err == nil {
		return sessionAncestryInfos(lineage), nil
	}

	sess, loadErr := s.canto.Load(ctx, sessionID)
	if loadErr != nil {
		return nil, fmt.Errorf("load session %s after lineage error: %w", sessionID, loadErr)
	}
	if len(sess.Events()) > 0 {
		return nil, fmt.Errorf("load session lineage %s: %w", sessionID, err)
	}
	return []SessionAncestryInfo{{
		SessionID: sessionID,
		Depth:     0,
		CreatedAt: info.CreatedAt,
	}}, nil
}

func (s *cantoStore) sessionBundleMetaExists(ctx context.Context, id string) (bool, error) {
	var existing string
	err := s.db.QueryRowContext(ctx, "SELECT id FROM session_meta WHERE id = ?", id).Scan(&existing)
	switch {
	case err == nil:
		return true, nil
	case errors.Is(err, sql.ErrNoRows):
		return false, nil
	default:
		return false, err
	}
}

func (s *cantoStore) cantoBundleSessionExists(ctx context.Context, id string) (bool, error) {
	if _, err := s.canto.Lineage(ctx, id); err == nil {
		return true, nil
	} else if !strings.Contains(err.Error(), "not found") {
		return false, err
	}
	sess, err := s.canto.Load(ctx, id)
	if err != nil {
		return false, err
	}
	if len(sess.Events()) > 0 {
		return true, nil
	}
	return false, nil
}

func (s *cantoStore) insertBundleSessionMeta(ctx context.Context, info SessionInfo) error {
	createdAt := info.CreatedAt
	if createdAt.IsZero() {
		createdAt = time.Now().UTC()
	}
	updatedAt := info.UpdatedAt
	if updatedAt.IsZero() {
		updatedAt = createdAt
	}
	_, err := s.db.ExecContext(
		ctx,
		`INSERT INTO session_meta
		 (id, cwd, model, branch, name, created_at, updated_at, last_preview)
		 VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
		info.ID,
		info.CWD,
		info.Model,
		info.Branch,
		info.Title,
		createdAt.Unix(),
		updatedAt.Unix(),
		info.LastPreview,
	)
	return err
}

func prepareSessionBundle(bundle SessionBundle) ([]preparedBundleRecord, error) {
	if err := bundle.verify(); err != nil {
		return nil, err
	}
	seen := make(map[string]struct{}, len(bundle.Sessions))
	prepared := make([]preparedBundleRecord, 0, len(bundle.Sessions))
	for _, record := range bundle.Sessions {
		id := strings.TrimSpace(record.Info.ID)
		if id == "" {
			return nil, fmt.Errorf("%w: session record missing id", ErrSessionBundleIntegrity)
		}
		if _, ok := seen[id]; ok {
			return nil, fmt.Errorf("%w: duplicate session %s", ErrSessionBundleIntegrity, id)
		}
		seen[id] = struct{}{}
		if record.Ancestry.SessionID != id {
			return nil, fmt.Errorf("%w: ancestry id %q does not match session %q",
				ErrSessionBundleIntegrity, record.Ancestry.SessionID, id)
		}
		if len(record.Events) != record.EventCount {
			return nil, fmt.Errorf("%w: session %s event count mismatch",
				ErrSessionBundleIntegrity, id)
		}
		if got := checksumBundleEvents(record.Events); got != record.EventChecksum {
			return nil, fmt.Errorf("%w: session %s event checksum mismatch",
				ErrSessionBundleIntegrity, id)
		}
		events := make([]csession.Event, 0, len(record.Events))
		for _, raw := range record.Events {
			event, err := csession.UnmarshalEventJSON(raw)
			if err != nil {
				return nil, fmt.Errorf("%w: decode session %s event: %v",
					ErrSessionBundleIntegrity, id, err)
			}
			if event.SessionID != id {
				return nil, fmt.Errorf("%w: event %s belongs to %s, want %s",
					ErrSessionBundleIntegrity, event.ID, event.SessionID, id)
			}
			events = append(events, event)
		}
		prepared = append(prepared, preparedBundleRecord{
			record:   record,
			ancestry: cantoAncestry(record.Ancestry),
			events:   events,
		})
	}
	if _, ok := seen[bundle.RootSessionID]; !ok {
		return nil, fmt.Errorf("%w: root session %s missing from bundle",
			ErrSessionBundleIntegrity, bundle.RootSessionID)
	}
	return prepared, nil
}

func (b *SessionBundle) seal() error {
	sum, err := b.computeChecksum()
	if err != nil {
		return err
	}
	b.Checksum = sum
	return nil
}

func (b SessionBundle) verify() error {
	if b.Version != sessionBundleVersion {
		return fmt.Errorf("%w: unsupported version %d", ErrSessionBundleIntegrity, b.Version)
	}
	if strings.TrimSpace(b.RootSessionID) == "" {
		return fmt.Errorf("%w: missing root session id", ErrSessionBundleIntegrity)
	}
	if len(b.Sessions) == 0 {
		return fmt.Errorf("%w: empty session bundle", ErrSessionBundleIntegrity)
	}
	got, err := b.computeChecksum()
	if err != nil {
		return err
	}
	if got != b.Checksum {
		return fmt.Errorf("%w: bundle checksum mismatch", ErrSessionBundleIntegrity)
	}
	return nil
}

func (b SessionBundle) computeChecksum() (string, error) {
	copy := b
	copy.Checksum = ""
	raw, err := json.Marshal(copy)
	if err != nil {
		return "", fmt.Errorf("marshal session bundle checksum payload: %w", err)
	}
	return "sha256:" + checksumHex(raw), nil
}

func checksumBundleEvents(events []json.RawMessage) string {
	h := sha256.New()
	for _, event := range events {
		h.Write(event)
		h.Write([]byte{'\n'})
	}
	return "sha256:" + hex.EncodeToString(h.Sum(nil))
}

func checksumHex(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}

func sessionAncestryInfos(records []csession.SessionAncestry) []SessionAncestryInfo {
	infos := make([]SessionAncestryInfo, 0, len(records))
	for _, record := range records {
		infos = append(infos, SessionAncestryInfo{
			SessionID:        record.SessionID,
			ParentSessionID:  record.ParentSessionID,
			ForkPointEventID: record.ForkPointEventID,
			BranchLabel:      record.BranchLabel,
			ForkReason:       record.ForkReason,
			Depth:            record.Depth,
			CreatedAt:        record.CreatedAt,
		})
	}
	return infos
}

func cantoAncestry(info SessionAncestryInfo) csession.SessionAncestry {
	return csession.SessionAncestry{
		SessionID:        info.SessionID,
		ParentSessionID:  info.ParentSessionID,
		ForkPointEventID: info.ForkPointEventID,
		BranchLabel:      info.BranchLabel,
		ForkReason:       info.ForkReason,
		Depth:            info.Depth,
		CreatedAt:        info.CreatedAt,
	}
}
