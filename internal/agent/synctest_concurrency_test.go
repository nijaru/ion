package agent

import (
	"context"
	"path/filepath"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

type blockingStream struct {
	ctx      context.Context
	started  chan struct{}
	canceled *atomic.Bool
}

func (s *blockingStream) Next() (*llm.Chunk, bool) {
	select {
	case <-s.started:
	default:
		close(s.started)
	}
	<-s.ctx.Done()
	s.canceled.Store(true)
	return nil, false
}

func (s *blockingStream) Err() error   { return s.ctx.Err() }
func (s *blockingStream) Close() error { return nil }

func TestControllerVirtualClockQueueAndTimeout(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		tempDir := t.TempDir()
		store, err := session.NewSQLiteStore(filepath.Join(tempDir, "sync-test.db"), "sync-session")
		if err != nil {
			t.Fatalf("failed to create sqlite store: %v", err)
		}
		defer store.Close()

		sess := session.NewSession(store, 16)
		durable, _ := any(store).(session.DurableStore)

		var requestCount atomic.Int32
		streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			requestCount.Add(1)
			return &mockStream{
				chunks: []*llm.Chunk{
					{Content: "Simulated streamed response in virtual time"},
					{StopReason: "stop"},
				},
			}, nil
		}

		ctrl := NewController(ControllerConfig{
			Session:       sess,
			Store:         store,
			Durable:       durable,
			StreamFn:      streamFn,
			Model:         llm.Model{ID: "virtual-test-model"},
			ApprovalMode:  ApprovalTrusted,
			QueueCapacity: 32,
		})
		defer ctrl.Close()

		ctx := context.Background()

		events := make(chan session.Event, 64)
		unsub := watchEvents(t, ctrl, func(event session.Event) {
			events <- event
		})
		defer unsub()

		// Prompt in virtual time
		_, err = ctrl.Prompt(ctx, "Hello in virtual time")
		if err != nil {
			t.Fatalf("Prompt failed: %v", err)
		}

		// Wait for turn completion event in virtual time
		for {
			event := <-events
			if _, ok := event.(session.AgentEnd); ok {
				break
			}
		}

		// Verify that the request completed
		if count := requestCount.Load(); count != 1 {
			t.Fatalf("requestCount = %d, want 1", count)
		}

		// Verify phase is PhaseReady
		if phase := ctrl.currentPhase(); phase != PhaseReady {
			t.Fatalf("ctrl.currentPhase = %v, want PhaseReady", phase)
		}
	})
}

func TestControllerVirtualClockSteerAndFollowUp(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		tempDir := t.TempDir()
		store, err := session.NewSQLiteStore(filepath.Join(tempDir, "sync-steer.db"), "sync-steer-session")
		if err != nil {
			t.Fatalf("failed to create sqlite store: %v", err)
		}
		defer store.Close()

		sess := session.NewSession(store, 16)
		durable, _ := any(store).(session.DurableStore)

		turnBlock := make(chan struct{})
		streamStarted := make(chan struct{})
		streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			close(streamStarted)
			select {
			case <-turnBlock:
			case <-ctx.Done():
				return nil, ctx.Err()
			}
			return &mockStream{
				chunks: []*llm.Chunk{
					{Content: "Stream completed"},
					{StopReason: "stop"},
				},
			}, nil
		}

		ctrl := NewController(ControllerConfig{
			Session:       sess,
			Store:         store,
			Durable:       durable,
			StreamFn:      streamFn,
			Model:         llm.Model{ID: "virtual-test-model"},
			ApprovalMode:  ApprovalTrusted,
			QueueCapacity: 8,
		})
		defer ctrl.Close()

		ctx := context.Background()

		events := make(chan session.Event, 64)
		unsub := watchEvents(t, ctrl, func(event session.Event) {
			events <- event
		})
		defer unsub()

		// Start prompt in concurrent goroutine
		go func() {
			_, _ = ctrl.Prompt(ctx, "First turn")
		}()

		// Wait until stream actually starts
		<-streamStarted

		// Inject steering while turn is streaming
		err = ctrl.Steer("Steering message 1")
		if err != nil {
			t.Fatalf("Steer failed: %v", err)
		}

		// Inject follow-up while turn is streaming
		err = ctrl.FollowUp("Follow-up message 1")
		if err != nil {
			t.Fatalf("FollowUp failed: %v", err)
		}

		// Release turn block
		close(turnBlock)

		// Wait for turn completion
		for {
			event := <-events
			if _, ok := event.(session.AgentEnd); ok {
				break
			}
		}
	})
}

func TestControllerVirtualClockAbortCascade(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		tempDir := t.TempDir()
		store, err := session.NewSQLiteStore(filepath.Join(tempDir, "sync-abort.db"), "sync-abort-session")
		if err != nil {
			t.Fatalf("failed to create sqlite store: %v", err)
		}
		defer store.Close()

		sess := session.NewSession(store, 16)
		durable, _ := any(store).(session.DurableStore)

		var canceled atomic.Bool
		streamStarted := make(chan struct{})
		streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &blockingStream{
				ctx:      ctx,
				started:  streamStarted,
				canceled: &canceled,
			}, nil
		}

		ctrl := NewController(ControllerConfig{
			Session:      sess,
			Store:        store,
			Durable:      durable,
			StreamFn:     streamFn,
			Model:        llm.Model{ID: "virtual-test-model"},
			ApprovalMode: ApprovalTrusted,
		})
		defer ctrl.Close()

		ctx := context.Background()

		// Start prompt in concurrent goroutine
		go func() {
			_, _ = ctrl.Prompt(ctx, "Turn to abort")
		}()

		// Wait until stream begins
		<-streamStarted

		// Abort
		_, _, err = ctrl.Abort()
		if err != nil {
			t.Fatalf("Abort failed: %v", err)
		}

		synctest.Wait()

		// Verify cancellation occurred
		if !canceled.Load() {
			t.Fatal("expected streamFn context to be canceled upon Abort")
		}

		// Verify phase returned to PhaseReady
		if phase := ctrl.currentPhase(); phase != PhaseReady {
			t.Fatalf("ctrl.currentPhase = %v, want PhaseReady", phase)
		}
	})
}

// TestControllerConcurrentRaceExecution tests multi-threaded OS concurrency
// and mutex lock contention under the real Go race detector.
func TestControllerConcurrentRaceExecution(t *testing.T) {
	tempDir := t.TempDir()
	store, err := session.NewSQLiteStore(filepath.Join(tempDir, "race.db"), "race-session")
	if err != nil {
		t.Fatalf("failed to create sqlite store: %v", err)
	}
	defer store.Close()

	sess := session.NewSession(store, 16)
	durable, _ := any(store).(session.DurableStore)

	streamFn := func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
		return &mockStream{
			chunks: []*llm.Chunk{
				{Content: "Race test response chunk"},
				{StopReason: "stop"},
			},
		}, nil
	}

	ctrl := NewController(ControllerConfig{
		Session:       sess,
		Store:         store,
		Durable:       durable,
		StreamFn:      streamFn,
		Model:         llm.Model{ID: "race-model"},
		ApprovalMode:  ApprovalTrusted,
		QueueCapacity: 64,
	})
	defer ctrl.Close()

	ctx := context.Background()
	var wg sync.WaitGroup

	// Launch multiple concurrent goroutines exercising prompt, steer, follow-up, and abort
	for i := 0; i < 5; i++ {
		wg.Add(1)
		go func(id int) {
			defer wg.Done()
			_, _ = ctrl.Prompt(ctx, "Prompt message")
			_ = ctrl.Steer("Concurrent steer")
			_ = ctrl.FollowUp("Concurrent follow up")
			_, _, _ = ctrl.Abort()
		}(i)
	}

	wg.Wait()
}
