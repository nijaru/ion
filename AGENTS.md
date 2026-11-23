# Ion development

- Keep one model-directed engine and one Session writer. Commit accepted input
  and observed effects before publishing them. An unknown effect is not safe to
  replay.
- This is unreleased v0. Replace obsolete APIs with their callers rather than
  adding compatibility shims.
- Build the Rust agent that makes daily coding work well, not a Pi clone.
  Use the [Ion implementation skill](.agents/skills/ion-reference-implementation/SKILL.md)
  before changing or reviewing behavior.

## Checks

Use the checks in [CI](.github/workflows/ci.yml) relevant to the change.
After building `ion`, `scripts/smoke.sh` exercises offline coding and reopen.
Use offline provider fixtures by default; get maintainer approval before live
model tests.
