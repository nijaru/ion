package tool

import (
	"context"
	"errors"
	"testing"
	"time"
)

func TestJobManagerCancellationBeforeStartReapsJob(t *testing.T) {
	manager := NewJobManager()
	defer manager.Close()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	_, err := manager.start(ctx, "never-started", func(ctx context.Context, _ func(int), _ func(localOutputUpdate) error) (string, error) {
		<-ctx.Done()
		return "", ctx.Err()
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("start error = %v, want context canceled", err)
	}
	jobs := manager.List()
	if len(jobs) != 0 {
		t.Fatalf("jobs = %#v, want no job for pre-canceled launch", jobs)
	}
}

func TestJobManagerCloseCancelsRunningJobs(t *testing.T) {
	manager := NewJobManager()
	started := make(chan struct{})
	jobID, err := manager.start(context.Background(), "long-running", func(ctx context.Context, signal func(int), _ func(localOutputUpdate) error) (string, error) {
		signal(123)
		close(started)
		<-ctx.Done()
		return "", ctx.Err()
	})
	if err != nil {
		t.Fatalf("start failed: %v", err)
	}
	<-started
	if err := manager.Close(); err != nil {
		t.Fatalf("close failed: %v", err)
	}
	job, err := manager.Get(jobID)
	if err != nil {
		t.Fatalf("get closed job: %v", err)
	}
	if job.Status != JobCanceled {
		t.Fatalf("closed job status = %s, want canceled", job.Status)
	}
	if err := manager.Close(); err != nil {
		t.Fatalf("second close failed: %v", err)
	}
}

func TestJobManagerBoundsConcurrentOutput(t *testing.T) {
	manager := NewJobManager()
	defer manager.Close()
	jobID, err := manager.start(context.Background(), "large-output", func(ctx context.Context, signal func(int), emit func(localOutputUpdate) error) (string, error) {
		signal(123)
		for i := 0; i < MaxToolOutputSize*4; i++ {
			if err := emit(localOutputUpdate{Text: "x"}); err != nil {
				return "", err
			}
			if i%1024 == 0 {
				time.Sleep(time.Microsecond)
			}
		}
		return "", nil
	})
	if err != nil {
		t.Fatalf("start failed: %v", err)
	}
	for {
		job, err := manager.Get(jobID)
		if err != nil {
			t.Fatalf("get job: %v", err)
		}
		if job.Status == JobCompleted {
			if len(job.Output) > MaxToolOutputSize+128 {
				t.Fatalf("job output length = %d, want bounded", len(job.Output))
			}
			if len(job.Output) == 0 {
				t.Fatal("job output is empty")
			}
			return
		}
		time.Sleep(time.Millisecond)
	}
}

func TestJobManagerBoundsCompletedHistory(t *testing.T) {
	manager := NewJobManager()
	defer manager.Close()
	for i := 0; i < maxRetainedJobs+8; i++ {
		if _, err := manager.start(context.Background(), "short", func(_ context.Context, signal func(int), _ func(localOutputUpdate) error) (string, error) {
			signal(123)
			return "done", nil
		}); err != nil {
			t.Fatalf("start %d failed: %v", i, err)
		}
	}
	if got := len(manager.List()); got != maxRetainedJobs {
		t.Fatalf("retained jobs = %d, want %d", got, maxRetainedJobs)
	}
	if _, err := manager.Get("job-1"); err == nil {
		t.Fatal("old completed job remained after history pruning")
	}
}
