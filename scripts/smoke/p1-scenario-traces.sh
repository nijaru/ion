#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TRACE="${ION_P1_TRACE:-$(mktemp "${TMPDIR:-/tmp}/ion-p1-scenario-trace.XXXXXX")}"

cd "$ROOT"
: >"$TRACE"
export ION_TMUX_TRACE="$TRACE"

trace_line() {
  local scenario="$1"
  local layer="$2"
  local status="$3"
  printf '{"scenario":"%s","layer":"%s","status":"%s"}\n' \
    "$scenario" "$layer" "$status" >>"$TRACE"
}

run_layer() {
  local scenario="$1"
  local layer="$2"
  shift 2

  printf '\n==> p1 scenario %s [%s]\n' "$scenario" "$layer"
  trace_line "$scenario" "$layer" "start"
  if "$@"; then
    trace_line "$scenario" "$layer" "pass"
    return 0
  fi
  trace_line "$scenario" "$layer" "fail"
  return 1
}

run_layer tool_provider_history provider \
  go test ./internal/agent \
    -run '^(TestHarnessIntegration_(ToolCalling|Steering|FollowUp)|TestDailyDriverSubmitToolPersistReplay)$' \
    -count=1 -timeout 180s

run_layer event_order_and_settlement event \
  go test ./app \
    -run '^(TestBusyInputControlErrorIsVisibleAndRestoresDraft|TestBusyInputFollowUpSuccessUsesRuntimeCommand|TestAwaitSessionEventDeduplicatesPendingSubscription)$' \
    -count=1 -timeout 180s

run_layer cancel_and_recovery event \
  go test ./app \
    -run '^(TestEscCancelsRunningTurn|TestCtrlCCancelsRunningTurn|TestTerminalCommitOwnsBubbleTeaPrintBoundary)$' \
    -count=1 -timeout 180s

run_layer resume_provider_history provider \
  go test ./internal/agent ./session \
    -run '^(TestHarnessIntegration_(DurableTurnCommitAndReplay|SessionResume)|TestSQLiteResumeSession)$' \
    -count=1 -timeout 180s

run_layer display_model display \
  go test ./test/tui \
    -run '^(TestDeterministicTUIAcceptance|TestDeterministicTUIAcceptanceApproval|TestDeterministicTUIAcceptanceCancelAndError|TestDeterministicTUIAcceptanceJobs|TestDeterministicTUIAcceptanceNarrowTerminal)$' \
    -count=1 -timeout 180s

run_layer timeout_surfacing timeout \
  go test ./cmd/ion ./tool \
    -run '^(TestPrintMode(CancelsTurnOnTimeout|TimeoutIsActionable)|TestBashTimeoutKillsProcessGroup)$' \
    -count=1 -timeout 180s

run_layer smoke_resume_persistence persistence \
  go test ./cmd/ion ./internal/agent \
    -run '^(TestPrintMode(UsesTheDurableRuntime|StructuredPrintModeUsesTheDurableRuntime)|TestDailyDriverSubmitToolPersistReplay)$' \
    -count=1 -timeout 180s

run_layer cli_exit_semantics cli \
  go test ./cmd/ion \
    -run '^TestRunCLIPrintWithoutProviderKeepsStdoutClean$' \
    -count=1 -timeout 180s

run_layer real_terminal_pty pty scripts/smoke/tmux-minimal-harness.sh

printf '\nP1 scenario trace passed: %s\n' "$TRACE"
