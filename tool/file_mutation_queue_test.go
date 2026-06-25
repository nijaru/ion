package tool

import (
	"sync"
	"sync/atomic"
	"testing"
)

func TestWithFileMutationQueueSerializes(t *testing.T) {
	var running atomic.Int32
	var maxConcurrent atomic.Int32

	var wg sync.WaitGroup
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			WithFileMutationQueue("/tmp/test-file.txt", func() (string, error) {
				n := running.Add(1)
				if n > maxConcurrent.Load() {
					maxConcurrent.Store(n)
				}
				// Simulate work
				for j := 0; j < 1000; j++ {
					_ = j
				}
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
	var running atomic.Int32
	var maxConcurrent atomic.Int32

	var wg sync.WaitGroup
	for i := 0; i < 10; i++ {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			WithFileMutationQueue("/tmp/test-file-"+string(rune('a'+idx))+".txt", func() (string, error) {
				n := running.Add(1)
				if n > maxConcurrent.Load() {
					maxConcurrent.Store(n)
				}
				for j := 0; j < 1000; j++ {
					_ = j
				}
				running.Add(-1)
				return "", nil
			})
		}(i)
	}
	wg.Wait()

	if maxConcurrent.Load() < 2 {
		t.Errorf("expected >1 concurrent operations for different files, got %d", maxConcurrent.Load())
	}
}
