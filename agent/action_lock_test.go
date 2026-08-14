package agent

import (
	"testing"
	"time"
)

func TestActionPathLocksOrderOverlappingPathsWithoutDeadlock(t *testing.T) {
	locks := newActionPathLocks()
	firstRelease := locks.acquire([]string{"b.txt", "a.txt"})
	secondAcquired := make(chan struct{})
	go func() {
		release := locks.acquire([]string{"a.txt", "b.txt"})
		release()
		close(secondAcquired)
	}()

	select {
	case <-secondAcquired:
		t.Fatal("overlapping action acquired locks before the first action released them")
	case <-time.After(20 * time.Millisecond):
	}
	firstRelease()
	select {
	case <-secondAcquired:
	case <-time.After(time.Second):
		t.Fatal("overlapping action remained blocked after release")
	}
	locks.mu.Lock()
	got := len(locks.entries)
	locks.mu.Unlock()
	if got != 0 {
		t.Fatalf("lock entries = %d, want cleanup", got)
	}
}
