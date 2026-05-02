package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"strings"

	"github.com/nijaru/ion/internal/storage"
)

type exportedSessionBundle struct {
	Bundle storage.SessionBundle
	Path   string
}

func exportSessionBundleFile(
	ctx context.Context,
	store storage.Store,
	sessionID string,
	path string,
) (exportedSessionBundle, error) {
	exporter, ok := store.(storage.SessionBundleExporter)
	if !ok {
		return exportedSessionBundle{}, fmt.Errorf("session store does not support export")
	}
	path = strings.TrimSpace(path)
	if path == "" || path == "-" {
		return exportedSessionBundle{}, fmt.Errorf("export path must be a file")
	}
	bundle, err := exporter.ExportSessionBundle(ctx, sessionID)
	if err != nil {
		return exportedSessionBundle{}, err
	}
	raw, err := json.MarshalIndent(bundle, "", "  ")
	if err != nil {
		return exportedSessionBundle{}, fmt.Errorf("marshal session bundle: %w", err)
	}
	raw = append(raw, '\n')
	if err := os.WriteFile(path, raw, 0o600); err != nil {
		return exportedSessionBundle{}, fmt.Errorf("write %s: %w", path, err)
	}
	return exportedSessionBundle{Bundle: bundle, Path: path}, nil
}

func importSessionBundleFile(
	ctx context.Context,
	store storage.Store,
	path string,
) ([]storage.SessionInfo, error) {
	importer, ok := store.(storage.SessionBundleImporter)
	if !ok {
		return nil, fmt.Errorf("session store does not support import")
	}
	path = strings.TrimSpace(path)
	if path == "" || path == "-" {
		return nil, fmt.Errorf("import path must be a file")
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	var bundle storage.SessionBundle
	if err := json.Unmarshal(raw, &bundle); err != nil {
		return nil, fmt.Errorf("decode session bundle %s: %w", path, err)
	}
	return importer.ImportSessionBundle(ctx, bundle)
}

func printSessionBundleExport(w io.Writer, exported exportedSessionBundle) {
	fmt.Fprintf(
		w,
		"Exported session %s to %s (%d sessions)\n",
		exported.Bundle.RootSessionID,
		exported.Path,
		len(exported.Bundle.Sessions),
	)
}

func printSessionBundleImport(w io.Writer, imported []storage.SessionInfo) {
	switch len(imported) {
	case 0:
		fmt.Fprintln(w, "Imported 0 sessions")
	case 1:
		fmt.Fprintf(w, "Imported session %s\n", imported[0].ID)
	default:
		fmt.Fprintf(w, "Imported %d sessions:\n", len(imported))
		for _, info := range imported {
			fmt.Fprintf(w, "- %s\n", info.ID)
		}
	}
}
