package main

import (
	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/tool"
)

type tuiJobController struct {
	manager *tool.JobManager
}

func (c tuiJobController) ListJobs() []app.JobInfo {
	if c.manager == nil {
		return nil
	}
	jobs := c.manager.List()
	out := make([]app.JobInfo, 0, len(jobs))
	for _, job := range jobs {
		out = append(out, app.JobInfo{
			ID:         job.ID,
			Command:    job.Command,
			Status:     string(job.Status),
			Output:     job.Output,
			Error:      job.Error,
			StartedAt:  job.StartedAt,
			FinishedAt: job.FinishedAt,
		})
	}
	return out
}

func (c tuiJobController) StopJob(id string) error {
	if c.manager == nil {
		return nil
	}
	return c.manager.Stop(id)
}
