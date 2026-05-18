#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SESSION="${ION_TMUX_SESSION:-ion-minimal-harness-smoke}"
WIDTH="${ION_TMUX_WIDTH:-180}"
HEIGHT="${ION_TMUX_HEIGHT:-30}"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ion-tmux-smoke.XXXXXX")"
CAPTURE="${ION_TMUX_CAPTURE:-$(mktemp "${TMPDIR:-/tmp}/ion-tmux-capture.XXXXXX")}"
ION_HOME="${ION_TMUX_HOME:-$HOME}"
LIVE="${ION_TMUX_LIVE:-0}"

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required" >&2
  exit 1
fi

cleanup() {
	tmux kill-session -t "$SESSION" 2>/dev/null || true
	rm -rf "$TMP_DIR"
}
trap cleanup EXIT

capture() {
  tmux capture-pane -t "$SESSION" -p -S -2000 >"$CAPTURE"
}

capture_visible() {
  tmux capture-pane -t "$SESSION" -p >"$CAPTURE"
}

assert_contains() {
  local needle="$1"
  capture
  if ! grep -Fq -- "$needle" "$CAPTURE"; then
    echo "missing expected text: $needle" >&2
    echo "--- capture ---" >&2
    cat "$CAPTURE" >&2
    exit 1
  fi
}

assert_not_contains() {
  local needle="$1"
  capture
  if grep -Fq -- "$needle" "$CAPTURE"; then
    echo "unexpected text: $needle" >&2
    echo "--- capture ---" >&2
    cat "$CAPTURE" >&2
    exit 1
  fi
}

assert_line_count_at_most() {
  local needle="$1"
  local max="$2"
  local count
  capture
  count="$(grep -Fxc -- "$needle" "$CAPTURE" || true)"
  if ((count > max)); then
    echo "too many lines matching $needle: got $count, want <= $max" >&2
    echo "--- capture ---" >&2
    cat "$CAPTURE" >&2
    exit 1
  fi
}

assert_separator_line_count_at_most() {
  local max="$1"
  local count
  capture
  count="$(grep -Ec '^─+$' "$CAPTURE" || true)"
  if ((count > max)); then
    echo "too many separator-only lines: got $count, want <= $max" >&2
    echo "--- capture ---" >&2
    cat "$CAPTURE" >&2
    exit 1
  fi
}

assert_visible_line_count_at_most() {
  local needle="$1"
  local max="$2"
  local count
  capture_visible
  count="$(grep -Fxc -- "$needle" "$CAPTURE" || true)"
  if ((count > max)); then
    echo "too many visible lines matching $needle: got $count, want <= $max" >&2
    echo "--- capture ---" >&2
    cat "$CAPTURE" >&2
    exit 1
  fi
}

assert_visible_contains() {
  local needle="$1"
  capture_visible
  if ! grep -Fq -- "$needle" "$CAPTURE"; then
    echo "missing expected visible text: $needle" >&2
    echo "--- capture ---" >&2
    cat "$CAPTURE" >&2
    exit 1
  fi
}

assert_visible_not_contains() {
  local needle="$1"
  capture_visible
  if grep -Fq -- "$needle" "$CAPTURE"; then
    echo "unexpected visible text: $needle" >&2
    echo "--- capture ---" >&2
    cat "$CAPTURE" >&2
    exit 1
  fi
}

assert_visible_separator_line_count_at_most() {
  local max="$1"
  local count
  capture_visible
  count="$(grep -Ec '^─+$' "$CAPTURE" || true)"
  if ((count > max)); then
    echo "too many visible separator-only lines: got $count, want <= $max" >&2
    echo "--- capture ---" >&2
    cat "$CAPTURE" >&2
    exit 1
  fi
}

wait_contains() {
  local needle="$1"
  local timeout="${2:-20}"
  local start
  start="$(date +%s)"
  while true; do
    capture
    if grep -Fq -- "$needle" "$CAPTURE"; then
      return 0
    fi
    if (($(date +%s) - start >= timeout)); then
      echo "timed out waiting for: $needle" >&2
      echo "--- capture ---" >&2
      cat "$CAPTURE" >&2
      exit 1
    fi
    sleep 0.5
  done
}

start_ion() {
  local args="${1:-}"
  tmux kill-session -t "$SESSION" 2>/dev/null || true
  tmux new-session \
    -d \
    -s "$SESSION" \
    -x "$WIDTH" \
    -y "$HEIGHT" \
    "cd \"$ROOT\" && HOME=\"$ION_HOME\" go run ./... $args"
  wait_contains "Type a message" 30
}

send_line() {
  tmux send-keys -t "$SESSION" "$1" Enter
  sleep "${ION_TMUX_STEP_DELAY:-1}"
}

send_multiline_composer_smoke() {
  tmux send-keys -t "$SESSION" "first line"
  sleep 0.2
  tmux send-keys -t "$SESSION" C-j
  sleep 0.2
  tmux send-keys -t "$SESSION" "second line"
  sleep "${ION_TMUX_STEP_DELAY:-1}"
  assert_contains "› first line"
  assert_contains "  second line"
  assert_not_contains "› second line"
  assert_not_contains "first linesecond line"
  tmux send-keys -t "$SESSION" C-c
  sleep 0.5
}

send_softwrap_composer_smoke() {
  tmux resize-window -t "$SESSION" -x 48 -y 24
  sleep 0.5
  tmux send-keys -t "$SESSION" "write a sentence that should wrap across multiple terminal rows before submit"
  sleep "${ION_TMUX_STEP_DELAY:-1}"
  assert_visible_contains "write a sentence"
  assert_visible_contains "before submit"
  tmux send-keys -t "$SESSION" C-c
  sleep 0.5
  tmux resize-window -t "$SESSION" -x "$WIDTH" -y "$HEIGHT"
  sleep 0.5
}

send_command_picker_filter_smoke() {
  tmux send-keys -t "$SESSION" "/" Tab
  sleep 0.5
  assert_visible_contains "Pick a command"
  tmux send-keys -t "$SESSION" "settings"
  sleep "${ION_TMUX_STEP_DELAY:-1}"
  assert_visible_contains "› /settings"
  tmux send-keys -t "$SESSION" Escape
  sleep 0.2
  tmux send-keys -t "$SESSION" C-c
  sleep 0.5
}

start_ion
assert_contains "ion v0.0.0"
assert_not_contains "Bash env inherited"
assert_not_contains "Env inherit"
assert_contains "Type a message"

send_multiline_composer_smoke
send_softwrap_composer_smoke
send_command_picker_filter_smoke

send_line "/provider"
assert_contains "Pick a provider"
assert_visible_not_contains "› /provider"
tmux send-keys -t "$SESSION" Escape
sleep 0.5

send_line "/help"
assert_contains "/tools"
assert_contains "/settings"

send_line "/tools"
assert_contains "Tools: 8"
assert_contains "bash env inherited"
assert_contains "bash, edit, glob, grep, list, multi_edit, read, write"
assert_not_contains "eager"
assert_not_contains "verify"

send_line "/jobs"
assert_contains "/jobs is deferred until its roadmap phase"

send_line "/settings"
assert_contains "thinking"
assert_contains "tool"

tmux resize-window -t "$SESSION" -x 84 -y 28
sleep 0.5
tmux resize-window -t "$SESSION" -x 60 -y 24
sleep 0.5
assert_contains "Type a message"
assert_visible_line_count_at_most "• Ready" 1
assert_visible_separator_line_count_at_most 2

if [[ "$LIVE" == "1" ]]; then
  send_line 'Use the bash tool exactly once to run `sleep 3; echo ion-tmux-smoke`, then reply with the single word done.'
  wait_contains "Bash(sleep 3; echo ion-tmux-smoke)" 90
  send_line "what happened?"
  wait_contains "Queued follow-up" 30
  wait_contains "› what happened?" 90
  wait_contains "Complete" 90

  start_ion "--continue"
  assert_contains "--- resumed ---"
  assert_contains "ion-tmux-smoke"
  send_line "Reply with exactly ok-continued if this resumed session contains ion-tmux-smoke, otherwise reply exactly ok-fresh."
  wait_contains "• ok-continued" 90
  wait_contains "Complete" 90
fi

capture
echo "tmux minimal harness smoke passed"
echo "capture: $CAPTURE"
