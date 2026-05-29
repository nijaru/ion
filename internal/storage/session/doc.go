// Package session provides Canto's durable append-only conversation log.
//
// A Session stores events as immutable facts and derives higher-level views
// from that log:
//   - Replayer reconstructs a Session from an event stream without re-emitting
//     writer or subscriber side effects.
//   - Rebuilder reconstructs the canonical post-compaction or projection
//     prompt view from durable snapshots plus later appended events.
//   - MessageAdded events are conversational transcript. ContextAdded events
//     are model-visible context, replayed as ordinary user-role context rather
//     than privileged instructions. Stable ContextAdded entries can be placed
//     before the transcript for prompt-cache reuse. Lifecycle, handoff,
//     approval, and audit events are durable but hidden from prompt history by
//     default.
//   - ProjectionSnapshotter appends durable rebuild checkpoints on time/count
//     policies so the rebuilder can fast-path cold starts and long transcripts.
//   - Messages returns the raw transcript exactly as messages were emitted.
//   - EffectiveMessages returns the model-visible history after durable
//     compaction snapshots are applied.
//   - EffectiveEntries returns the same model-visible history together with
//     originating event IDs and context markers when known.
//   - Artifact events carry durable artifact descriptors and provenance rather
//     than embedding artifact bodies directly in the log.
//   - RecordArtifact records external descriptors. The artifact package owns
//     body storage helpers that can persist new artifacts while emitting
//     artifact_recorded events.
//
// Forked sessions preserve lineage by minting fresh event IDs and recording
// fork_origin metadata that points back to the parent session and event.
// Compaction and projection snapshots are persisted as append-only events, so
// replay and resumed runs see the same effective prompt history the model saw.
//
// Stores may also expose first-class session-tree queries through
// SessionTreeStore, so callers can navigate parent/child/lineage
// relationships without scanning copied event payloads.
package session
