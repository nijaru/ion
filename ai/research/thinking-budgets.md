# Thinking Budget Research

**Date**: 2026-01-19

## Current Implementation

ion has 3 levels: Off, Standard (4k), Extended (16k)

## Claude Defaults

- Default: 4,096 tokens
- Keywords: `low` (1k), `medium` (4k), `high` (8k-16k range)
- Max: 32,768 tokens

## Questions

1. **Should thinking default to on for models that support it?**
   - Pros: Better reasoning quality
   - Cons: More tokens, slower, more expensive
   - Suggestion: Default off, but prompt user on first use of thinking model?

2. **What levels make sense?**
   - Current: Off, 1k, 4k, 16k
   - Alternative: Off, 4k, 10k, 16k, 32k
   - Claude Code uses: default (4k), extended (16k?)

3. **Model detection**
   - Need to detect which models support extended thinking
   - Claude: claude-3-5-sonnet, claude-sonnet-4, opus models
   - OpenRouter: Check model capabilities?

## Suggested Implementation

| Level    | Tokens | Use Case                 |
| -------- | ------ | ------------------------ |
| Off      | 0      | Simple tasks, speed      |
| Default  | 4,096  | Standard reasoning       |
| Extended | 10,240 | Complex problems         |
| Deep     | 16,384 | Very complex reasoning   |
| Max      | 32,768 | Maximum reasoning (rare) |

Or simpler 3-level:

- Off
- Standard (4k)
- Extended (16k)

## Status Line Display

Current: `[low]`, `[med]`, `[high]`
Alternative: Show token budget? `[think:4k]`
