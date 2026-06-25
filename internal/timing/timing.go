// Package timing provides startup profiling instrumentation.
// Enable with ION_TIMING=1 environment variable.
package timing

import (
	"fmt"
	"os"
	"sync"
	"time"
)

var (
	enabled  bool
	mu       sync.Mutex
	points   []point
	lastTime time.Time
)

type point struct {
	label string
	ms    int64
}

func init() {
	enabled = os.Getenv("ION_TIMING") == "1"
	if enabled {
		lastTime = time.Now()
	}
}

// Reset clears all recorded timing points.
func Reset() {
	if !enabled {
		return
	}
	mu.Lock()
	defer mu.Unlock()
	points = points[:0]
	lastTime = time.Now()
}

// Record records a timing point with the given label.
func Record(label string) {
	if !enabled {
		return
	}
	mu.Lock()
	defer mu.Unlock()
	now := time.Now()
	points = append(points, point{
		label: label,
		ms:    now.Sub(lastTime).Milliseconds(),
	})
	lastTime = now
}

// Print prints all recorded timing points to stderr.
func Print() {
	if !enabled || len(points) == 0 {
		return
	}
	mu.Lock()
	defer mu.Unlock()
	fmt.Fprintln(os.Stderr, "\n--- Startup Timings ---")
	var total int64
	for _, p := range points {
		fmt.Fprintf(os.Stderr, "  %s: %dms\n", p.label, p.ms)
		total += p.ms
	}
	fmt.Fprintf(os.Stderr, "  TOTAL: %dms\n", total)
	fmt.Fprintln(os.Stderr, "------------------------")
}
