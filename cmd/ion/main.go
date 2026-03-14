package main

import (
	"context"
	"flag"
	"fmt"
	"os"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/fake"
	"github.com/nijaru/ion/internal/backend/native"
)

func main() {
	backendFlag := flag.String("backend", "fake", "Backend to use (fake, native)")
	flag.Parse()

	var b backend.Backend
	switch *backendFlag {
	case "native":
		b = native.New()
	case "fake":
		b = fake.New()
	default:
		fmt.Fprintf(os.Stderr, "invalid backend: %s\n", *backendFlag)
		os.Exit(1)
	}

	ctx := context.Background()
	if err := b.Session().Open(ctx); err != nil {
		fmt.Fprintf(os.Stderr, "backend initialization error: %v\n", err)
		os.Exit(1)
	}

	p := tea.NewProgram(app.New(b))
	if _, err := p.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "ion-go error: %v\n", err)
		os.Exit(1)
	}
}
