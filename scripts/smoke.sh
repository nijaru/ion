#!/usr/bin/env bash
# Offline executable check of the real headless coding and reopen path.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE=debug
if [[ "${1:-}" == --release ]]; then PROFILE=release; shift; fi
[[ $# == 0 ]] || { echo 'usage: scripts/smoke.sh [--release]' >&2; exit 2; }
BIN="${ION_SMOKE_BIN:-$ROOT/target/$PROFILE/ion}"
if [[ -z "${ION_SMOKE_BIN:-}" ]]; then
    if [[ "$PROFILE" == release ]]; then
        cargo build --quiet --locked --release -p ion
    else
        cargo build --quiet --locked -p ion
    fi
fi

WORK="$(mktemp -d "${ION_SMOKE_TMPDIR:-/tmp}/ion-smoke.XXXXXX")"
server_pid=
cleanup() {
    if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi
    if [[ "${ION_SMOKE_KEEP:-0}" == 1 ]]; then echo "smoke files: $WORK" >&2; else rm -rf "$WORK"; fi
}
trap cleanup EXIT
mkdir -p "$WORK/workspace" "$WORK/config" "$WORK/state"
printf 'sample data\n' > "$WORK/workspace/data.txt"
export XDG_CONFIG_HOME="$WORK/config" XDG_STATE_HOME="$WORK/state"
if "$BIN" --cwd "$WORK/workspace" inspect > "$WORK/missing.out" 2> "$WORK/missing.err"; then
    echo 'inspect accepted a nonexistent session' >&2; exit 1
fi
[[ ! -e "$WORK/state/ion" ]] || { echo 'read-only inspect created session state' >&2; exit 1; }
python3 "$ROOT/scripts/smoke_provider.py" "$WORK/port" "$WORK/requests" > "$WORK/server.out" 2> "$WORK/server.err" &
server_pid=$!
for _ in {1..100}; do [[ -s "$WORK/port" ]] && break; sleep 0.05; done
[[ -s "$WORK/port" ]] || { cat "$WORK/server.err" >&2; echo 'mock provider did not start' >&2; exit 1; }
port="$(cat "$WORK/port")"

"$BIN" use smoke smoke-model --endpoint "http://127.0.0.1:$port/v1/chat/completions" --wire chat-completions > "$WORK/use.out"
"$BIN" --cwd "$WORK/workspace" run 'Read data.txt, edit it, create created.txt, then verify both files with shell.' > "$WORK/first.out" 2> "$WORK/first.err"
[[ "$(cat "$WORK/workspace/data.txt")" == 'sample data updated' ]]
[[ "$(cat "$WORK/workspace/created.txt")" == 'created by ion' ]]
grep -q 'TASK_COMPLETE' "$WORK/first.out"
"$BIN" --cwd "$WORK/workspace" --continue inspect > "$WORK/first.json"

"$BIN" --cwd "$WORK/workspace" --continue run 'What did we finish previously?' > "$WORK/second.out" 2> "$WORK/second.err"
grep -q 'RESUMED' "$WORK/second.out"
"$BIN" --cwd "$WORK/workspace" --continue inspect > "$WORK/second.json"
python3 - "$WORK/first.json" "$WORK/second.json" "$WORK/requests" <<'PY'
import json, sys
first, second = (json.load(open(path)) for path in sys.argv[1:3])
assert first['unfinished_turn'] is None and second['unfinished_turn'] is None
assert len(first['entries']) == 11, first['entries']
assert len(second['entries']) == 14, second['entries']
assert [entry['kind'] for entry in first['entries']].count('tool_result') == 4
assert [entry['kind'] for entry in second['entries']].count('turn_ended') == 2
requests = [json.loads(line) for line in open(sys.argv[3])]
assert len(requests) == 6, len(requests)
assert len([m for m in requests[-1]['messages'] if m['role'] == 'user']) == 2
PY

# An explicit session owns its original working directory. A conflicting
# --cwd must fail before model or tool work instead of silently targeting it.
mkdir "$WORK/other-workspace"
session_db="$(find "$WORK/state/ion/sessions" -name '*.sqlite' -print -quit)"
[[ -n "$session_db" ]]
if "$BIN" --cwd "$WORK/other-workspace" --session "$session_db" run 'Do not run this' \
    > "$WORK/wrong-cwd.out" 2> "$WORK/wrong-cwd.err"; then
    echo 'conflicting --cwd was accepted for an existing session' >&2; exit 1
fi
grep -q -- '--cwd does not match the session' "$WORK/wrong-cwd.err"
if "$BIN" --cwd "$WORK/other-workspace" --session "$session_db" inspect \
    > "$WORK/wrong-inspect.out" 2> "$WORK/wrong-inspect.err"; then
    echo 'conflicting --cwd was accepted for inspect' >&2; exit 1
fi
grep -q -- '--cwd does not match the session' "$WORK/wrong-inspect.err"
"$BIN" --session "$session_db" inspect > "$WORK/after-wrong-cwd.json"
cmp "$WORK/second.json" "$WORK/after-wrong-cwd.json"
"$BIN" --cwd "$WORK/workspace" --session "$session_db" clone > "$WORK/clone.out"
clone_id="$(awk '{print $NF}' "$WORK/clone.out")"
"$BIN" --cwd "$WORK/workspace" --session "$clone_id" inspect > "$WORK/clone.json"
python3 - "$WORK/second.json" "$WORK/clone.json" <<'PY'
import json, sys
source, clone = (json.load(open(path)) for path in sys.argv[1:])
assert source['entries'] == clone['entries']
assert clone['cwd'] == source['cwd']
PY
"$BIN" --session "$session_db" inspect > "$WORK/source-after-clone.json"
cmp "$WORK/second.json" "$WORK/source-after-clone.json"

kill "$server_pid" 2>/dev/null || true
wait "$server_pid" 2>/dev/null || true
server_pid=
python3 "$ROOT/scripts/smoke_provider.py" "$WORK/json-port" "$WORK/json-requests" > "$WORK/json-server.out" 2> "$WORK/json-server.err" &
server_pid=$!
for _ in {1..100}; do [[ -s "$WORK/json-port" ]] && break; sleep 0.05; done
[[ -s "$WORK/json-port" ]] || { cat "$WORK/json-server.err" >&2; echo 'JSON mock provider did not start' >&2; exit 1; }
"$BIN" use smoke smoke-model --endpoint "http://127.0.0.1:$(cat "$WORK/json-port")/v1/chat/completions" --wire chat-completions > "$WORK/json-use.out"
printf 'sample data\n' > "$WORK/other-workspace/data.txt"
"$BIN" --json --cwd "$WORK/other-workspace" run 'Read data.txt, edit it, create created.txt, then verify both files with shell.' > "$WORK/events.jsonl" 2> "$WORK/events.err"
python3 - "$WORK/events.jsonl" "$WORK/other-workspace" <<'PY'
import json, pathlib, sys
events = [json.loads(line) for line in open(sys.argv[1])]
assert events[0]['type'] == 'session' and pathlib.Path(events[0]['cwd']) == pathlib.Path(sys.argv[2]).resolve()
assert events[-1] == {'type': 'run_end', 'status': 'completed'}
started = [event['call_id'] for event in events if event['type'] == 'tool_started']
finished = [event['call_id'] for event in events if event['type'] == 'tool_finished']
assert len(started) == 4 and started == finished
assert next(event['text'] for event in events if event['type'] == 'final') == 'TASK_COMPLETE'
assert pathlib.Path(sys.argv[2], 'data.txt').read_text() == 'sample data updated\n'
assert pathlib.Path(sys.argv[2], 'created.txt').read_text() == 'created by ion\n'
PY
mkdir "$WORK/error-workspace"
if "$BIN" --json --cwd "$WORK/error-workspace" run 'Unexpected prompt' > "$WORK/error-events.jsonl" 2> "$WORK/error-events.err"; then
    echo 'JSON mode accepted a failed model request' >&2; exit 1
fi
python3 - "$WORK/error-events.jsonl" <<'PY'
import json, sys
events = [json.loads(line) for line in open(sys.argv[1])]
assert events[0]['type'] == 'session'
assert events[-1]['type'] == 'run_end' and events[-1]['status'] == 'failed'
assert 'provider returned HTTP 500' in events[-1]['error']
PY
echo 'Ion offline headless coding and session reopen: OK'
