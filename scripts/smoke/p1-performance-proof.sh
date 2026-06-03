#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COUNT="${ION_PERF_COUNT:-3}"
HYPERFINE_RUNS="${ION_PERF_HYPERFINE_RUNS:-10}"

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

phase_note() {
  printf '\n-- %s\n' "$*"
}

cd "$ROOT"

phase_note "environment"
run go version
if command -v sw_vers >/dev/null 2>&1; then
  run sw_vers
fi
if command -v system_profiler >/dev/null 2>&1; then
  system_profiler SPHardwareDataType | sed -n '1,18p'
fi

phase_note "startup/status readiness via smoke binary"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ion-p1-perf.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT
SMOKE_BIN="$TMP_DIR/ion-tui-smoke"
SMOKE_STORE="$TMP_DIR/startup-store"
run go build -o "$SMOKE_BIN" ./internal/smoke
if command -v hyperfine >/dev/null 2>&1; then
  run hyperfine --warmup 3 --runs "$HYPERFINE_RUNS" --shell=none \
    --prepare "rm -rf '$SMOKE_STORE' && mkdir -p '$SMOKE_STORE'" \
    "$SMOKE_BIN --startup-check --store $SMOKE_STORE"
else
  "$SMOKE_BIN" --startup-check --store "$SMOKE_STORE"
  printf 'hyperfine not found; startup-check ran once without statistical timing\n'
fi

phase_note "TUI reducer/render hot paths"
run go test ./internal/app -run '^$' \
  -bench 'Benchmark(P1StartupReadyShell|P1EventToViewActiveTool|P1BurstAgentDeltaReduction|View|Render|Ranked)' \
  -benchmem -count "$COUNT"

phase_note "storage replay/projection and session-list paths"
run go test ./internal/storage -run '^$' \
  -bench 'BenchmarkCantoStore(DisplayProjection|ListSessions)' \
  -benchmem -count "$COUNT"

phase_note "prompt and P1 tool budget"
run go test ./internal/backend/canto -run TestPromptPreludeBudgetReport -count=1 -v

cat <<'EOF'

P1 performance proof finished.

Record this output in ai/review/p1-performance-proof-2026-05-23.md with the
machine, run count, datasets, and any follow-up tasks for regressions.
EOF
