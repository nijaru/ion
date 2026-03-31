# Canto Manual Compaction Update

Date: 2026-03-26

## Summary

`../canto` now exposes a framework-level helper for manual durable compaction:

```go
result, err := context.CompactSession(ctx, provider, model, sess, context.CompactOptions{
    MaxTokens:    maxTokens,
    ThresholdPct: thresholdPct,
    MinKeepTurns: minKeepTurns,
    Artifacts:    artifactStore,
})
```

It returns:

```go
type CompactResult struct {
    Compacted bool
}
```

## Why it mattered

Ion did not need to change compaction semantics.

This note is archived because it is framework-reference material, not active project-state memory.
