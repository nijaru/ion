package main

import (
	"fmt"
	"os"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/go-host/internal/app"
	"github.com/nijaru/ion/go-host/internal/backend/fake"
)

func main() {
	p := tea.NewProgram(app.New(fake.New()))
	if _, err := p.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "ion-go error: %v\n", err)
		os.Exit(1)
	}
}
