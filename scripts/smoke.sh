#!/usr/bin/env bash
# Daily-driver smoke checklist (tk-670r): the flows a maintainer hits on
# every real session, driven through a real terminal (tmux) against the
# built binary with an isolated data root. Run this before any
# readiness claim; green unit gates alone are not readiness evidence.
#
# Usage: scripts/smoke.sh [--release]
# Requires: tmux, python3 (sqlite3 module), cargo.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug/ion"
SESSION="ion-smoke"
STEP=0

if [[ "${1:-}" == "--release" ]]; then
    BIN="$ROOT/target/release/ion"
fi

WORK="$(mktemp -d /tmp/ion-smoke.XXXXXX)"
mkdir -p "$WORK/data/ion"
printf '' > "$WORK/settings.toml"

cleanup() {
    tmux kill-session -t "$SESSION" 2>/dev/null
    # Kill only the ion this script launched (child of our panes).
    [[ -n "${SMOKE_PID:-}" ]] && pkill -9 -P "$SMOKE_PID" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT

pass() { STEP=$((STEP + 1)); echo "ok $STEP - $1"; }
fail() { STEP=$((STEP + 1)); echo "FAIL $STEP - $1"; tmux capture-pane -t "$SESSION" -p 2>/dev/null | tail -20; exit 1; }

capture() { tmux capture-pane -t "$SESSION" -p 2>/dev/null; }

wait_for() { # $1 needle, $2 timeout seconds
    local deadline=$((SECONDS + ${2:-15}))
    until capture | grep -q "$1"; do
        (( SECONDS > deadline )) && return 1
        sleep 0.2
    done
}

wait_for_idle() { # $1 timeout seconds
    local deadline=$((SECONDS + ${1:-15}))
    local screen
    while (( SECONDS <= deadline )); do
        screen="$(capture)"
        # The footer is the PTY-visible completion boundary. It is
        # current-screen state, unlike streamed response text, which
        # may already be present while OperationFinished is pending.
        if grep -Eq '^[[:space:]]+ion \([^)]*\)[[:space:]]*$' <<<"$screen" \
            && ! grep -Eq '^[[:space:]]+ion \([^)]*\)[[:space:]]+●[[:space:]]' <<<"$screen"
        then
            return 0
        fi
        sleep 0.2
    done
    return 1
}

launch() { # $@ = ion args
    tmux kill-session -t "$SESSION" 2>/dev/null
    # Explicit bash: tmux default-shell may be fish, where "$?" aborts.
    # Keep-alive keeps the exit code visible after ion exits.
    tmux new-session -d -s "$SESSION" -x 100 -y 30 \
        "bash -c 'env ION_SETTINGS=$WORK/settings.toml XDG_DATA_HOME=$WORK/data $BIN $* 2>$WORK/stderr.log; printf \"SMOKE_EXIT=%s\\n\" \$?; sleep 60'"
    SMOKE_PID="$(tmux display-message -p -t "$SESSION" '#{pane_pid}')"
}

ion_child_pid() {
    # Pane pid is the tmux shell wrapper; ion is its child or grandchild.
    local pid parent grandparent
    for pid in $(pgrep -x ion); do
        parent="$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')"
        if [[ "$parent" == "$SMOKE_PID" ]]; then echo "$pid"; return 0; fi
        grandparent="$(ps -o ppid= -p "$parent" 2>/dev/null | tr -d ' ')"
        if [[ "$grandparent" == "$SMOKE_PID" ]]; then echo "$pid"; return 0; fi
    done
}

quit_and_check_exit_code() { # $1 = description
    wait_for_idle 10 || fail "$1: ion did not reach an idle footer"
    tmux send-keys -t "$SESSION" C-d
    local deadline=$((SECONDS + 10))
    until capture | grep -q "SMOKE_EXIT="; do
        (( SECONDS > deadline )) && fail "$1: ion did not exit after ctrl+d"
        sleep 0.2
    done
    capture | grep -q "SMOKE_EXIT=0" || fail "$1: exit code was not 0: $(capture | grep SMOKE_EXIT)"
}

type_line() { tmux send-keys -t "$SESSION" -l "$1"; tmux send-keys -t "$SESSION" Enter; }

echo "== building =="
cargo build -q -p ion || { echo "build failed"; exit 1; }
[[ -x "$BIN" ]] || { echo "binary missing at $BIN"; exit 1; }

echo "== 1. fresh start =="
launch
wait_for "escape to interrupt" 15 || fail "fresh start: no idle banner"
pass "idle banner renders"

echo "== 2. submit a turn =="
type_line "hello"
wait_for "scripted provider" 15 || fail "turn: no scripted response"
first_count=$(capture | grep -c "scripted provider")
sleep 1
second_count=$(capture | grep -c "scripted provider")
[[ "$first_count" == "$second_count" ]] || fail "turn: response duplicated ($first_count -> $second_count)"
pass "turn committed exactly once"

echo "== 3. clean exit =="
quit_and_check_exit_code "clean exit"
pass "ctrl+d quits with code 0"

echo "== 4. resume shows persisted history =="
launch "--resume"
wait_for "resumed" 15 || fail "resume: no resumed banner"
capture | grep -qE "(> hello|hello)" || fail "resume: previous turn missing"
pass "resume restores history"

echo "== 5. kill -9 mid-operation recovers =="
# Relaunch with the scripted provider held open so the operation is
# deterministically in flight when the process dies.
tmux kill-session -t "$SESSION" 2>/dev/null
tmux new-session -d -s "$SESSION" -x 100 -y 30 \
    "bash -c 'env ION_SETTINGS=$WORK/settings.toml XDG_DATA_HOME=$WORK/data ION_TEST_PROVIDER_DELAY_MS=8000 $BIN --resume 2>$WORK/stderr.log; printf \"SMOKE_EXIT=%s\\n\" \$?; sleep 60'"
SMOKE_PID="$(tmux display-message -p -t "$SESSION" '#{pane_pid}')"
wait_for "resumed" 15 || fail "kill -9: no resumed banner"
type_line "interruptible"
wait_for "> interruptible" 10 || fail "kill -9: submission not accepted"
CHILD="$(ion_child_pid)"
[[ -n "$CHILD" ]] && kill -9 "$CHILD" || fail "kill -9: no ion child found"
launch "--resume"
wait_for "resumed" 15 || fail "kill -9: no resumed banner after crash"
# Valid recoveries: the open model step either surfaces as
# indeterminate/cancelled, or replays safely against the fresh provider
# and completes. Either way nothing is lost and the session is usable.
if ! capture | grep -qE "indeterminate|cancelled"; then
    capture | grep -q "scripted provider:" \
        || fail "kill -9: interrupted op neither surfaced nor replayed"
fi
type_line "/help"
wait_for "/compact" 10 || fail "kill -9: composer unusable after recovery"
pass "interrupted operation settles and session stays usable"

echo "== 6. older schema store archives instead of refusing =="
tmux kill-session -t "$SESSION" 2>/dev/null
python3 - <<PYEOF || { echo "python3/sqlite3 unavailable"; exit 1; }
import sqlite3
conn = sqlite3.connect("$WORK/data/ion/sessions.db")
conn.execute("CREATE TABLE IF NOT EXISTS sessions (id TEXT)")
conn.execute("PRAGMA user_version = 6")
conn.commit()
conn.close()
PYEOF
launch
wait_for "archived your old session store" 15 || fail "schema bump: archive notice not shown"
wait_for "escape to interrupt" 15 || fail "schema bump: session did not start"
ls "$WORK/data/ion" | grep -q "\.v6\..*\.bak" || fail "schema bump: no .bak archive created"
pass "old store archived, notice shown, session starts"

echo "== 7. resize storm stays interactive =="
for _ in 1 2 3 4; do
    tmux resize-window -t "$SESSION" -x 40 -y 15
    sleep 0.05
    tmux resize-window -t "$SESSION" -x 100 -y 30
    sleep 0.05
done
type_line "still here"
wait_for "still here" 10 || fail "resize storm: input lost"
pass "composer survives resize storm"

echo "== 8. final clean exit =="
quit_and_check_exit_code "post-storm exit"
pass "clean exit after storm"

echo
echo "ALL $STEP CHECKS PASSED — safe to ask for maintainer dogfood."
