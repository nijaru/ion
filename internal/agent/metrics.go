package agent

import (
	"sync"
	"sync/atomic"
	"time"
)

// Metrics collects runtime statistics for turn latency, token usage, and errors.
// All counters are safe for concurrent use.
type Metrics struct {
	TurnCount   atomic.Int64
	ErrorCount  atomic.Int64
	TotalTokens atomic.Int64 // sum of prompt + completion tokens
	TotalCost   float64      // estimated cost in USD (approximate)

	latencyBuckets [10]atomic.Int64 // histogram: <100ms, <200ms, <400ms, <800ms, <1.6s, <3.2s, <6.4s, <12.8s, <25.6s, >25.6s
	mu             sync.Mutex      // protects TotalCost updates
}

// RecordTurn increments the turn counter and records latency in the histogram.
func (m *Metrics) RecordTurn(dur time.Duration) {
	m.TurnCount.Add(1)
	ms := dur.Milliseconds()
	switch {
	case ms < 100:
		m.latencyBuckets[0].Add(1)
	case ms < 200:
		m.latencyBuckets[1].Add(1)
	case ms < 400:
		m.latencyBuckets[2].Add(1)
	case ms < 800:
		m.latencyBuckets[3].Add(1)
	case ms < 1600:
		m.latencyBuckets[4].Add(1)
	case ms < 3200:
		m.latencyBuckets[5].Add(1)
	case ms < 6400:
		m.latencyBuckets[6].Add(1)
	case ms < 12800:
		m.latencyBuckets[7].Add(1)
	case ms < 25600:
		m.latencyBuckets[8].Add(1)
	default:
		m.latencyBuckets[9].Add(1)
	}
}

// RecordError increments the error counter.
func (m *Metrics) RecordError() {
	m.ErrorCount.Add(1)
}

// RecordTokens records token usage.
func (m *Metrics) RecordTokens(input, output int) {
	m.TotalTokens.Add(int64(input + output))
}

// LogLatencySnapshot returns bucket counts as a map of boundary (ms) → count.
func (m *Metrics) LatencySnapshot() map[int]int64 {
	return map[int]int64{
		100:   m.latencyBuckets[0].Load(),
		200:   m.latencyBuckets[1].Load(),
		400:   m.latencyBuckets[2].Load(),
		800:   m.latencyBuckets[3].Load(),
		1600:  m.latencyBuckets[4].Load(),
		3200:  m.latencyBuckets[5].Load(),
		6400:  m.latencyBuckets[6].Load(),
		12800: m.latencyBuckets[7].Load(),
		25600: m.latencyBuckets[8].Load(),
		-1:    m.latencyBuckets[9].Load(), // -1 = overflow bucket
	}
}

// Snapshot returns a summary of all metrics.
func (m *Metrics) Snapshot() MetricsSnapshot {
	return MetricsSnapshot{
		TurnCount:   m.TurnCount.Load(),
		ErrorCount:  m.ErrorCount.Load(),
		TotalTokens: m.TotalTokens.Load(),
	}
}

// MetricsSnapshot is a point-in-time copy of metrics for public consumption.
type MetricsSnapshot struct {
	TurnCount   int64 `json:"turn_count"`
	ErrorCount  int64 `json:"error_count"`
	TotalTokens int64 `json:"total_tokens"`
}
