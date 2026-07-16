package app

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
)

func (m Model) handleJobsCommand(fields []string) (Model, tea.Cmd) {
	if m.Model.Jobs == nil {
		return m, cmdError("background jobs are unavailable")
	}
	switch {
	case len(fields) == 1:
		jobs := m.Model.Jobs.ListJobs()
		if len(jobs) == 0 {
			return m, m.terminalCommit().Help("No background jobs.")
		}
		var output strings.Builder
		for _, job := range jobs {
			fmt.Fprintf(&output, "%s  %-9s %s", job.ID, job.Status, job.Command)
			if job.Error != "" {
				fmt.Fprintf(&output, " (error: %s)", job.Error)
			}
			if job.Output != "" {
				lastLine := job.Output
				if lines := strings.Split(strings.TrimRight(lastLine, "\n"), "\n"); len(lines) > 0 {
					lastLine = lines[len(lines)-1]
				}
				if strings.TrimSpace(lastLine) != "" {
					fmt.Fprintf(&output, "\n  %s", lastLine)
				}
			}
			output.WriteByte('\n')
		}
		return m, m.terminalCommit().Help(strings.TrimRight(output.String(), "\n"))
	case len(fields) == 3 && strings.EqualFold(fields[1], "stop"):
		id := strings.TrimSpace(fields[2])
		if id == "" {
			return m, cmdError("usage: /jobs stop <job-id>")
		}
		if err := m.Model.Jobs.StopJob(id); err != nil {
			return m, cmdError(err.Error())
		}
		return m, m.terminalCommit().Help("Stopped background job " + id + ".")
	default:
		return m, cmdError("usage: /jobs [stop <job-id>]")
	}
}
