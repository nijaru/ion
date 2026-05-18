#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

phase_note() {
  printf '\n-- %s\n' "$*"
}

cd "$ROOT"

phase_note "deterministic core gates"
run go test ./... -count=1 -timeout 300s
run go vet ./...

phase_note "TUI tmux gate"
run env \
  "ION_TMUX_HOME=${ION_TMUX_HOME:-$(mktemp -d "${TMPDIR:-/tmp}/ion-phase1-home.XXXXXX")}" \
  scripts/smoke/tmux-minimal-harness.sh

if [[ "${ION_PHASE1_RACE:-0}" == "1" ]]; then
  phase_note "race subset"
  run go test -race \
    ./cmd/ion \
    ./internal/app \
    ./internal/backend/canto \
    ./internal/backend/canto/tools \
    ./internal/config \
    ./internal/providers \
    ./internal/storage \
    -count=1 -timeout 300s
else
  phase_note "race subset skipped; set ION_PHASE1_RACE=1 for the Phase 1 exit gate"
fi

if [[ "${ION_PHASE1_LIVE:-0}" == "1" ]]; then
  phase_note "live backend/provider gate"
  run go test ./cmd/ion -run TestLiveSmokeTurnAndToolCall -count=1 -timeout 180s -v

  phase_note "live TUI/provider gate"
  run env ION_TMUX_LIVE=1 scripts/smoke/tmux-minimal-harness.sh
else
  phase_note "live gates skipped; set ION_PHASE1_LIVE=1 for the Phase 1 exit gate"
fi

cat <<'EOF'

Phase 1 acceptance wrapper finished.

This is not a completion claim unless race and live gates were included and the
scenario matrix in ai/review/phase-1-architecture-reset-2026-05-18.md is logged
as covered.
EOF
