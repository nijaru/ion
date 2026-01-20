# Research: Fuzzy Search Crates (fuzzy-matcher vs nucleo)

## Summary

We need fuzzy search for provider/model selection now and for filenames, commands, and settings later. `fuzzy-matcher` is lightweight and easy to integrate; `nucleo` is more sophisticated but heavier and carries an MPL-2.0 license. Given scope, `fuzzy-matcher` is a good default; revisit `nucleo` if matching quality becomes a pain point.

## Key Findings

### fuzzy-matcher

- Crate: `fuzzy-matcher`
- Docs: https://docs.rs/fuzzy-matcher
- Repo: https://github.com/lotabout/fuzzy-matcher
- License: MIT
- Simpler API, good for small-to-medium lists

### nucleo

- Crate: `nucleo`
- Docs: https://docs.rs/nucleo/0.5.0
- Repo: https://github.com/helix-editor/nucleo
- License: MPL-2.0
- Designed for high-performance matching (used by Helix)

## Recommendation

- **Default**: `fuzzy-matcher` for provider/model/command/filename search.
- **Upgrade path**: consider `nucleo` if match quality or performance becomes limiting.

## Sources

- docs.rs: fuzzy-matcher https://docs.rs/fuzzy-matcher
- docs.rs: nucleo 0.5.0 https://docs.rs/nucleo/0.5.0
