// Package prompt builds model requests from durable session state.
//
// The package has two phases:
//   - Builder.BuildPreview shapes the in-flight request without mutating
//     durable state.
//   - Builder.Build and Builder.BuildCommit run commit-time mutation first and
//     then rebuild the request from the updated session state.
//
// RequestProcessor handles preview-safe shaping and ContextMutator handles
// durable changes such as compaction or artifact recording.
//
// History is always derived from session.EffectiveMessages rather than the raw
// transcript, so compaction and projection checkpoints stay durable across
// future turns. Offloader and Summarizer persist compaction snapshots back
// into the session log; projection snapshotting keeps cold-start rebuilds from
// scanning the full transcript again. Offloader also emits durable artifact
// descriptors for externalized content.
// BudgetGuard reports request-capacity state before the provider call and gives
// later masking/rebuild layers a structured budget status instead of relying on
// overflow strings.
// ObservationMasker is the reversible masking tier: it rewrites oversized tool
// outputs into traceable placeholders in the in-flight request while leaving
// the append-only event log untouched.
// CompactSession provides a small consumer-neutral helper for manually running
// the built-in offload-then-summarize compaction path and reporting whether
// durable compaction occurred. LazyTools derives unlocked tool state from
// prior search_tools completions recorded in the session.
package prompt
