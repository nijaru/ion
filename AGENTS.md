# Ion development

- Keep one model-directed engine and one Session writer. Commit accepted input
  and observed effects before publishing them. An unknown effect is not safe to
  replay.
- This is unreleased v0. Replace obsolete APIs with their callers rather than
  adding compatibility shims.
- Pi is the default reference, not a parity target. Judge each design choice
  on what it does for Ion and how it fits Rust.
- For changes to runtime, Session, tool, or terminal contracts, see the
  [Ion implementation skill](.agents/skills/ion-reference-implementation/SKILL.md).

## Checks

Use the checks in [CI](.github/workflows/ci.yml) relevant to the change.
After building `ion`, `scripts/smoke.sh` exercises offline coding and reopen.
Use offline provider fixtures by default; get maintainer approval before live
model tests.
