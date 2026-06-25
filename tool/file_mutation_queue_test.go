package tool

import (
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestWithFileMutationQueueSerializes(t *testing.T) {
	var running atomic.Int32
	var maxConcurrent atomic.Int32

	var wg sync.WaitGroup
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			WithFileMutationQueue("/tmp/test-serialize-file.txt", func() (string, error) {
				n := running.Add(1)
				if n > maxConcurrent.Load() {
					maxConcurrent.Store(n)
				}
				time.Sleep(5 * time.Millisecond)
				running.Add(-1)
				return "", nil
			})
		}()
	}
	wg.Wait()

	if maxConcurrent.Load() > 1 {
		t.Errorf("expected max 1 concurrent operation, got %d", maxConcurrent.Load())
	}
}

func TestWithFileMutationQueueParallelDifferentFiles(t *testing.T) {
	startBarrier := make(chan struct{})
	var entered atomic.Int32
	var maxConcurrent atomic.Int32
	var running atomic.Int32

	var wg sync.WaitGroup
	for i := 0; i < 5; i++ {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			WithFileMutationQueue("/tmp/test-parallel-"+string(rune('a'+idx))+".txt", func() (string, error) {
				n := running.Add(1)
				if n > maxConcurrent.Load() {
					maxConcurrent.Store(n)
				}
				entered.Add(1)
				// Wait until all goroutines have entered their critical section
				for entered.Load() < 3 {
					time.Sleep(time.Millisecond)
				}
				time.Sleep(5 * time.Millisecond)
				running.Add(-1)
				return "", nil
			})
		}(i)
	}
	wg.Wait()

	if maxConcurrent.Load() < 2 {
		t.Errorf("expected >1 concurrent operations for different files, got %d", maxConcurrent.Load())
	}
	_ = startBarrier // unused but keeps the linter happy
}
