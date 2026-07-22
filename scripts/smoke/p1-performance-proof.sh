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
run go build -o "$SMOKE_BIN" ./test/tui
if command -v hyperfine >/dev/null 2>&1; then
  run hyperfine --warmup 3 --runs "$HYPERFINE_RUNS" --shell=none \
    --prepare "rm -rf '$SMOKE_STORE' && mkdir -p '$SMOKE_STORE'" \
    "$SMOKE_BIN --startup-check --store $SMOKE_STORE"
else
  "$SMOKE_BIN" --startup-check --store "$SMOKE_STORE"
  printf 'hyperfine not found; startup-check ran once without statistical timing\n'
fi

phase_note "TUI render hot paths"
run go test ./app -run '^$' \
  -bench '^Benchmark(ModelViewReadyShell|ModelViewStreamingTranscript|RenderMarkdownLongDocument)$' \
  -benchmem -count "$COUNT"

phase_note "runtime first-event snapshot"
run go test ./internal/agent -run '^$' \
  -bench '^BenchmarkControllerSubscribeSnapshot256$' \
  -benchmem -count "$COUNT"

phase_note "session persistence and branch navigation"
run go test ./session -run '^$' \
  -bench '^BenchmarkSQLite(Branch256|BuildContext256)$' \
  -benchmem -count "$COUNT"

cat <<'EOF'

Ion performance baseline finished.

The current baseline covers startup readiness, runtime snapshot/first-event
latency, TUI shell/stream rendering, Markdown rendering, SQLite branch
navigation, and context reconstruction. Provider latency, tool dispatch,
compaction, subprocess teardown, and clean shutdown remain separate
measurements until current representative benchmarks exist.

Record this output with the machine, run count, datasets, and any follow-up
tasks for regressions.
EOF
