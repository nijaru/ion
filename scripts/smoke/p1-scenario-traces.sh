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
  go test ./internal/backend/canto \
    -run '^(TestSubmitTurnExecutes(ReadFindAndGrep|LsWriteAndEdit)FirstMinutesFlow|TestSubmitTurnToolFailurePersistsForFollowUp|TestRetryRecoveryWaitsThroughToolLoop)$' \
    -count=1 -timeout 180s

run_layer event_order_and_settlement event \
  go test ./internal/backend/canto \
    -run '^(TestSubmitTurnEmitsSavePointBeforeTurnFinished|TestTranslateRunEventProjectsCantoToolLifecycle|TestTranslateRunEventUsesCantoUsageAfterToolCompleted|TestTranslateEventsPreservesToolCompletedError)$' \
    -count=1 -timeout 180s

run_layer cancel_and_recovery event \
  go test ./internal/backend/canto \
    -run '^(TestSubmitTurnCancelDuringToolSuppressesLateToolEvents|TestSubmitTurnProviderErrorLeavesBackendReusable|TestBackendCancelClearsQueuedSteering)$' \
    -count=1 -timeout 180s

run_layer resume_provider_history provider \
  go test ./internal/backend/canto \
    -run '^(TestResumedToolSessionSendsValidFollowUpHistory|TestProviderHistory(ExcludesIonDisplayOnlyEvents|RecoversToolContentPartsFromLifecycle)|TestRetryRecoveryWaitsThroughToolLoop)$' \
    -count=1 -timeout 180s

run_layer display_model display \
  go test ./internal/app \
    -run '^(TestP1InlineScenarioMatrix|TestMinimalHarnessAcceptanceFinalStateAndReplay|TestCoreLoopSmoke(CancelPersistsTerminalEntry|ProviderLimitErrorPersistsForResume|RetryStatusPersistsForResume|BudgetCancellationPersistsForResume))$' \
    -count=1 -timeout 180s

run_layer timeout_surfacing timeout \
  go test ./cmd/ion ./internal/backend/canto ./internal/tools \
    -run '^(TestPrintModeCancelsTurnOnTimeout|TestSubmitTurnUsesCallerContext|TestBashTimeoutKillsProcessGroup)$' \
    -count=1 -timeout 180s

run_layer smoke_resume_persistence persistence \
  go test ./cmd/ion-tui-smoke ./internal/storage \
    -run '^(TestSmokeBackendPersistsNativeTranscriptForResume|TestCantoStoreDisplayReplaySharesProviderHistorySource)$' \
    -count=1 -timeout 180s

run_layer real_terminal_pty pty scripts/smoke/tmux-minimal-harness.sh

printf '\nP1 scenario trace passed: %s\n' "$TRACE"
