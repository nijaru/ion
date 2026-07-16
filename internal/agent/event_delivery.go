package agent

import (
	"sync"
	"sync/atomic"

	"github.com/nijaru/ion/session"
)

// eventDelivery is the single ordered delivery path for harness events. Emitters
// enqueue without waiting for the consumer channel; one dispatcher invokes
// listeners in order and a second sender drains the same ordered sequence to the
// channel. A slow or temporarily absent TUI therefore cannot drop events or
// stall the agent turn.
type eventDelivery struct {
	output      chan session.Event
	closeOutput bool

	mu          sync.Mutex
	cond        *sync.Cond
	queue       []queuedEvent
	head        int
	closed      bool
	stop        chan struct{}
	stopped     chan struct{}
	dispatching atomic.Int32

	outputMu     sync.Mutex
	outputCond   *sync.Cond
	outputQueue  []queuedOutput
	outputHead   int
	outputClosed bool
	senderDone   chan struct{}
	closeOnce    sync.Once
}

type queuedEvent struct {
	event         session.Event
	listeners     []func(session.Event)
	listenersDone chan struct{}
	outputDone    chan struct{}
	listenerPanic any
}

type queuedOutput struct {
	event      session.Event
	outputDone chan struct{}
}

func newEventDelivery(output chan session.Event, closeOutput bool) *eventDelivery {
	d := &eventDelivery{
		output:      output,
		closeOutput: closeOutput,
		stop:        make(chan struct{}),
		stopped:     make(chan struct{}),
		senderDone:  make(chan struct{}),
	}
	d.cond = sync.NewCond(&d.mu)
	d.outputCond = sync.NewCond(&d.outputMu)
	go d.dispatch()
	go d.send()
	return d
}

// enqueue adds an event and waits until listeners have observed it. When the
// buffered output path has immediate room and no queued backlog, it also waits
// for this event to reach the channel. That preserves follow-up ordering for
// channel consumers without making an unread or full channel stall the agent.
func (d *eventDelivery) enqueue(event session.Event, listeners []func(session.Event)) {
	item := queuedEvent{
		event:         event,
		listeners:     listeners,
		listenersDone: make(chan struct{}),
		outputDone:    make(chan struct{}),
	}
	waitForOutput := d.outputHasRoom()

	d.mu.Lock()
	if d.closed {
		d.mu.Unlock()
		return
	}
	d.queue = append(d.queue, item)
	d.cond.Signal()
	d.mu.Unlock()

	// A listener may enqueue a follow-up event. The dispatcher must not wait
	// for that nested event while it is still executing the current callback.
	if d.dispatching.Load() == 0 {
		<-item.listenersDone
		if waitForOutput {
			select {
			case <-item.outputDone:
			case <-d.stop:
			}
		}
	}
	if item.listenerPanic != nil {
		panic(item.listenerPanic)
	}
}

// enqueueAsync preserves queue order without waiting for listeners. It is used
// by setters while the harness mutex is held so a reentrant listener can safely
// call back into the harness.
func (d *eventDelivery) enqueueAsync(event session.Event, listeners []func(session.Event)) {
	d.mu.Lock()
	if !d.closed {
		d.queue = append(d.queue, queuedEvent{
			event:         event,
			listeners:     listeners,
			listenersDone: make(chan struct{}),
			outputDone:    make(chan struct{}),
		})
		d.cond.Signal()
	}
	d.mu.Unlock()
}

func (d *eventDelivery) dispatch() {
	defer close(d.stopped)
	for {
		item, ok := d.next()
		if !ok {
			return
		}

		d.dispatching.Add(1)
		func() {
			defer func() {
				if recovered := recover(); recovered != nil {
					item.listenerPanic = recovered
				}
				close(item.listenersDone)
			}()
			for _, listener := range item.listeners {
				listener(item.event)
			}
		}()
		d.dispatching.Add(-1)
		if item.listenerPanic != nil {
			continue
		}
		d.enqueueOutput(item)
	}
}

func (d *eventDelivery) next() (queuedEvent, bool) {
	d.mu.Lock()
	defer d.mu.Unlock()
	for d.head == len(d.queue) && !d.closed {
		d.cond.Wait()
	}
	if d.head == len(d.queue) {
		return queuedEvent{}, false
	}
	item := d.queue[d.head]
	d.head++
	if d.head == len(d.queue) {
		d.queue = d.queue[:0]
		d.head = 0
	} else if d.head >= 1024 && d.head*2 >= len(d.queue) {
		copy(d.queue, d.queue[d.head:])
		d.queue = d.queue[:len(d.queue)-d.head]
		d.head = 0
	}
	return item, true
}

func (d *eventDelivery) outputHasRoom() bool {
	if cap(d.output) == 0 {
		return false
	}
	d.outputMu.Lock()
	defer d.outputMu.Unlock()
	return !d.outputClosed && len(d.output) < cap(d.output) && len(d.outputQueue) == 0
}

func (d *eventDelivery) enqueueOutput(item queuedEvent) {
	d.outputMu.Lock()
	if !d.outputClosed {
		d.outputQueue = append(d.outputQueue, queuedOutput{
			event:      item.event,
			outputDone: item.outputDone,
		})
		d.outputCond.Signal()
	}
	d.outputMu.Unlock()
}

func (d *eventDelivery) send() {
	defer close(d.senderDone)
	for {
		item, ok := d.nextOutput()
		if !ok {
			return
		}
		select {
		case d.output <- item.event:
			close(item.outputDone)
		case <-d.stop:
			return
		}
	}
}

func (d *eventDelivery) nextOutput() (queuedOutput, bool) {
	d.outputMu.Lock()
	defer d.outputMu.Unlock()
	for d.outputHead == len(d.outputQueue) && !d.outputClosed {
		d.outputCond.Wait()
	}
	if d.outputClosed {
		return queuedOutput{}, false
	}
	item := d.outputQueue[d.outputHead]
	d.outputHead++
	if d.outputHead == len(d.outputQueue) {
		d.outputQueue = d.outputQueue[:0]
		d.outputHead = 0
	} else if d.outputHead >= 1024 && d.outputHead*2 >= len(d.outputQueue) {
		copy(d.outputQueue, d.outputQueue[d.outputHead:])
		d.outputQueue = d.outputQueue[:len(d.outputQueue)-d.outputHead]
		d.outputHead = 0
	}
	return item, true
}

// close cancels blocked channel sends and waits for both dispatcher goroutines.
// Events may remain undelivered to an unread channel only after explicit
// harness shutdown; active turns never drop them.
func (d *eventDelivery) close() {
	d.closeOnce.Do(func() {
		d.mu.Lock()
		d.closed = true
		for i := d.head; i < len(d.queue); i++ {
			close(d.queue[i].listenersDone)
		}
		d.queue = d.queue[:0]
		d.head = 0
		d.cond.Broadcast()
		d.mu.Unlock()

		d.outputMu.Lock()
		d.outputClosed = true
		d.outputCond.Broadcast()
		d.outputMu.Unlock()
		close(d.stop)

		<-d.stopped
		<-d.senderDone
		if d.closeOutput {
			close(d.output)
		}
	})
}
