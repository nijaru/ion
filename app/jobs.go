package app

import "time"

// JobInfo is the app-layer projection of a runtime-owned background command.
// It intentionally contains no process handles or persistence fields.
type JobInfo struct {
	ID         string
	Command    string
	Status     string
	Output     string
	Error      string
	StartedAt  time.Time
	FinishedAt time.Time
}

type JobController interface {
	ListJobs() []JobInfo
	StopJob(id string) error
}
