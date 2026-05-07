package canto

import (
	"context"
	"fmt"

	ionsession "github.com/nijaru/ion/internal/session"
)

func (b *Backend) Jobs() []ionsession.JobInfo {
	b.mu.Lock()
	bash := b.bash
	b.mu.Unlock()
	if bash == nil {
		return nil
	}

	jobs := bash.Jobs()
	out := make([]ionsession.JobInfo, 0, len(jobs))
	for _, job := range jobs {
		out = append(out, ionsession.JobInfo{
			ID:          job.ID,
			Command:     job.Command,
			Status:      job.Status,
			OutputBytes: job.OutputBytes,
		})
	}
	return out
}

func (b *Backend) StopJob(ctx context.Context, id string) (string, error) {
	b.mu.Lock()
	bash := b.bash
	b.mu.Unlock()
	if bash == nil {
		return "", fmt.Errorf("background jobs are unavailable before session open")
	}
	return bash.StopJob(ctx, id)
}
