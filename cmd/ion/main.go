package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto"
	"github.com/nijaru/ion/internal/backend/native"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/storage"
)

// version is set at build time via -ldflags "-X main.version=vX.Y.Z".
var version = "dev"

func main() {
	continueFlag := flag.Bool(
		"continue",
		false,
		"Continue the most recent session in this directory",
	)
	resumeFlag := flag.String("resume", "", "Resume a specific session by ID")
	backendFlag := flag.String("backend", "canto", "Backend to use (canto, native)")
	flag.Parse()

	// Initialize storage
	home, _ := os.UserHomeDir()
	storageRoot := filepath.Join(home, ".ion")
	store, err := storage.NewCantoStore(storageRoot)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to initialize storage: %v\n", err)
		os.Exit(1)
	}

	var b backend.Backend
	switch *backendFlag {
	case "native":
		b = native.New()
	default:
		b = canto.New()
	}
	b.SetStore(store)

	// Load config
	cfg, err := config.Load()
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to load config: %v\n", err)
		os.Exit(1)
	}
	b.SetConfig(cfg)

	ctx := context.Background()
	cwd, _ := os.Getwd()

	var sess storage.Session
	var sessionID string

	if *resumeFlag != "" {
		sessionID = *resumeFlag
	} else if *continueFlag {
		recent, err := store.GetRecentSession(ctx, cwd)
		if err == nil && recent != nil {
			sessionID = recent.ID
		}
	}

	if sessionID != "" {
		sess, err = store.ResumeSession(ctx, sessionID)
		if err != nil {
			fmt.Fprintf(os.Stderr, "failed to resume session %s: %v\n", sessionID, err)
			os.Exit(1)
		}
		b.SetSession(sess)
		if err := b.Session().Resume(ctx, sessionID); err != nil {
			fmt.Fprintf(os.Stderr, "backend resume error: %v\n", err)
			os.Exit(1)
		}
	} else {
		// Open new session
		modelName := cfg.Model
		if cfg.Provider != "" && !strings.Contains(modelName, "/") {
			modelName = cfg.Provider + " " + cfg.Model
		}

		if modelName == "" {
			fmt.Fprintf(os.Stderr, "ION_MODEL environment variable or config.toml must be set (e.g. 'openrouter minimax/minimax-m2.7')\n")
			os.Exit(1)
		}

		sess, err = store.OpenSession(ctx, cwd, modelName, currentBranch())
		if err != nil {
			fmt.Fprintf(os.Stderr, "failed to open session: %v\n", err)
			os.Exit(1)
		}
		b.SetSession(sess)
		if err := b.Session().Open(ctx); err != nil {
			fmt.Fprintf(os.Stderr, "backend initialization error: %v\n", err)
			os.Exit(1)
		}
	}

	p := tea.NewProgram(app.New(b, sess, cwd, currentBranch(), version))
	if _, err := p.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "ion error: %v\n", err)
		os.Exit(1)
	}
}

func currentBranch() string {
	out, err := exec.Command("git", "rev-parse", "--abbrev-ref", "HEAD").Output()
	if err != nil {
		return "unknown"
	}
	return strings.TrimSpace(string(out))
}
