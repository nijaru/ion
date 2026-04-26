package main

import (
	"errors"
	"os"

	"github.com/nijaru/canto/workspace"
)

func loadEscalationConfig(cwd string) (*workspace.EscalationConfig, error) {
	root, err := workspace.Open(cwd)
	if err != nil {
		return nil, err
	}
	defer root.Close()

	cfg, err := workspace.LoadEscalate(root)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, nil
		}
		return nil, err
	}
	return cfg, nil
}
