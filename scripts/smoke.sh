#!/usr/bin/env bash
# Offline executable smoke for the maintained headless Session path.
# This proves durable submit/reopen/idempotence and zero provider starts when
# credentials are absent. It is NOT live-provider or terminal qualification.
# Usage: scripts/smoke.sh [--release] (or ION_SMOKE_BIN=/path/to/ion ...)
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
WORK="$(mktemp -d /tmp/ion-smoke.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
mkdir "$WORK/state" "$WORK/workspace"
printf 'sample data\n' > "$WORK/workspace/data.txt"

common=(--state "$WORK/state" --workspace "$WORK/workspace"
        --endpoint https://api.example.test/v1/chat/completions
        --api-key-env ION_SMOKE_ABSENT_KEY)
run=("${common[@]}" --model gpt-test --model-input-limit 8192
     --model-output-limit 2048 --request-key exact-key 'Read data.txt')

if env -u ION_SMOKE_ABSENT_KEY "$BIN" run "${run[@]}" > "$WORK/first.json" 2> "$WORK/first.err"; then
    echo 'FAIL: absent credentials allowed dispatch' >&2; exit 1
fi
grep -q 'MissingCredentials' "$WORK/first.err"
"$BIN" inspect --state "$WORK/state" > "$WORK/snapshot.json"
TURN="$(python3 - "$WORK/snapshot.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
assert s['unfinished_turn'], 'submission did not create a durable Turn'
assert s['model_attempts'] == [], 'preflight consumed a physical provider attempt'
assert s['tool_attempts'] == [], 'preflight started a tool'
assert len(s['transcript_tail']) == 1, 'user input did not project exactly once'
print(s['unfinished_turn']['id'])
PY
)"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" run "${run[@]}" > "$WORK/replay.json" 2> "$WORK/replay.err"; then
    echo 'FAIL: absent credentials allowed replay dispatch' >&2; exit 1
fi
grep -q 'MissingCredentials' "$WORK/replay.err"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" resume "${common[@]}" --turn "$TURN" > "$WORK/resume.json" 2> "$WORK/resume.err"; then
    echo 'FAIL: absent credentials allowed resume dispatch' >&2; exit 1
fi
grep -q 'MissingCredentials' "$WORK/resume.err"
"$BIN" inspect --state "$WORK/state" > "$WORK/reopened.json"
python3 - "$WORK/snapshot.json" "$WORK/reopened.json" <<'PY'
import json, sys
a, b = (json.load(open(path)) for path in sys.argv[1:])
assert a == b, 'passive reopen, idempotent submit, or blocked resume changed durable state'
PY

# The second wire API must pass through the same Session and credential preflight.
mkdir "$WORK/anthropic-state"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" run --state "$WORK/anthropic-state" \
    --workspace "$WORK/workspace" --wire anthropic-messages \
    --endpoint https://api.anthropic.com/v1/messages \
    --api-key-env ION_SMOKE_ABSENT_KEY --model claude-test \
    --model-input-limit 8192 --model-output-limit 2048 \
    'synthetic, no network' > "$WORK/anthropic.json" 2> "$WORK/anthropic.err"; then
    echo 'FAIL: absent Anthropic credentials allowed dispatch' >&2; exit 1
fi
grep -q 'MissingCredentials' "$WORK/anthropic.err"
"$BIN" inspect --state "$WORK/anthropic-state" > "$WORK/anthropic-snapshot.json"
python3 - "$WORK/anthropic-snapshot.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
assert s['model_attempts'] == [], 'Anthropic preflight consumed a provider attempt'
assert s['config']['config']['providers'][0]['id'] == 'anthropic-messages'
PY
anthropic_turn="$(python3 - "$WORK/anthropic-snapshot.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))['unfinished_turn']['id'])
PY
)"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" resume --state "$WORK/anthropic-state" \
    --workspace "$WORK/workspace" --wire chat-completions \
    --endpoint https://api.anthropic.com/v1/messages \
    --api-key-env ION_SMOKE_ABSENT_KEY --turn "$anthropic_turn" \
    > "$WORK/mismatched-wire.json" 2> "$WORK/mismatched-wire.err"; then
    echo 'FAIL: an existing Session changed its frozen wire API' >&2; exit 1
fi
grep -q 'host wire API or endpoint differs' "$WORK/mismatched-wire.err"
"$BIN" inspect --state "$WORK/anthropic-state" > "$WORK/anthropic-after-mismatch.json"
cmp "$WORK/anthropic-snapshot.json" "$WORK/anthropic-after-mismatch.json"

# A serialized request that exceeds its frozen byte budget never reaches egress.
mkdir "$WORK/byte-state"
oversized_prompt="$(python3 -c 'print("X" * 8192)')"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" run --state "$WORK/byte-state" \
    --workspace "$WORK/workspace" --endpoint https://api.example.test/v1/chat/completions \
    --api-key-env ION_SMOKE_ABSENT_KEY --model gpt-test \
    --model-input-limit 8192 --model-output-limit 2048 \
    --max-request-bytes 4096 "$oversized_prompt" > "$WORK/byte.json" 2> "$WORK/byte.err"; then
    echo 'FAIL: oversized request reached provider dispatch' >&2; exit 1
fi
grep -q 'ContextCapacity' "$WORK/byte.err"
"$BIN" inspect --state "$WORK/byte-state" > "$WORK/byte-snapshot.json"
python3 - "$WORK/byte-snapshot.json" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
assert s['model_attempts'] == [], 'request byte ceiling consumed a provider attempt'
assert s['config']['config']['context']['max_request_bytes'] == 4096
PY

mkdir "$WORK/workspace/state"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" run --state "$WORK/workspace/state" \
    --workspace "$WORK/workspace" --endpoint https://api.example.test/v1/chat/completions \
    --model gpt-test --model-input-limit 8192 --model-output-limit 2048 \
    'reject in-workspace state' > "$WORK/unsafe.out" 2> "$WORK/unsafe.err"; then
    echo 'FAIL: writable workspace accepted as host state' >&2; exit 1
fi
grep -q 'outside the writable workspace' "$WORK/unsafe.err"
[[ -z "$(find "$WORK/workspace/state" -mindepth 1 -maxdepth 1 -print -quit)" ]]
mkdir "$WORK/state/registry/agent-root"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" run --state "$WORK/state" \
    --workspace "$WORK/state/registry/agent-root" \
    --endpoint https://api.example.test/v1/chat/completions \
    --model gpt-test --model-input-limit 8192 --model-output-limit 2048 \
    'reject workspace inside registry' > "$WORK/registry-inside.out" 2> "$WORK/registry-inside.err"; then
    echo 'FAIL: a registry child was accepted as agent workspace' >&2; exit 1
fi
grep -q 'registry namespace must be disjoint' "$WORK/registry-inside.err"
mkdir "$WORK/alias-state" "$WORK/workspace/alias-target"
ln -s "$WORK/workspace/alias-target" "$WORK/alias-state/registry"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" run --state "$WORK/alias-state" \
    --workspace "$WORK/workspace" --endpoint https://api.example.test/v1/chat/completions \
    --model gpt-test --model-input-limit 8192 --model-output-limit 2048 \
    'reject registry alias' > "$WORK/alias.out" 2> "$WORK/alias.err"; then
    echo 'FAIL: registry alias into workspace was accepted' >&2; exit 1
fi
grep -q 'registry resolves into the writable workspace' "$WORK/alias.err"
[[ -z "$(find "$WORK/workspace/alias-target" -mindepth 1 -maxdepth 1 -print -quit)" ]]
echo 'headless offline smoke passed (not live-provider or terminal qualification)'
