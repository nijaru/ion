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
WORK="$(mktemp -d "${ION_SMOKE_TMPDIR:-/tmp}/ion-smoke.XXXXXX")"
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

# An endpoint path is part of the frozen provider binding, not merely its HTTPS
# origin. A same-origin path change must fail before any provider request.
if env -u ION_SMOKE_ABSENT_KEY "$BIN" resume --state "$WORK/state" \
    --workspace "$WORK/workspace" --endpoint https://api.example.test/v1/other \
    --api-key-env ION_SMOKE_ABSENT_KEY --turn "$TURN" \
    > "$WORK/wrong-endpoint.out" 2> "$WORK/wrong-endpoint.err"; then
    echo 'FAIL: same-origin endpoint path changed a frozen provider' >&2; exit 1
fi
grep -q 'host wire API or endpoint differs' "$WORK/wrong-endpoint.err"
"$BIN" inspect --state "$WORK/state" > "$WORK/wrong-endpoint.snapshot"
cmp "$WORK/snapshot.json" "$WORK/wrong-endpoint.snapshot"

# An optional monetary ceiling cannot dispatch without a trusted operator's
# all-in quote. A quote exceeding that ceiling parks before provider egress;
# changing the frozen ceiling when replaying the same request is refused.
mkdir "$WORK/priced-state"
priced_common=(--state "$WORK/priced-state" --workspace "$WORK/workspace"
               --endpoint https://api.example.test/v1/chat/completions
               --api-key-env ION_SMOKE_SYNTHETIC_KEY)
priced_run=("${priced_common[@]}" --model gpt-test --model-input-limit 8192
            --model-output-limit 2048 --max-cost-microusd 25
            --request-key priced-key 'synthetic priced request')
if ION_SMOKE_SYNTHETIC_KEY=synthetic "$BIN" run "${priced_run[@]}" \
    > "$WORK/priced.out" 2> "$WORK/priced.err"; then
    echo 'FAIL: capped request dispatched without pricing' >&2; exit 1
fi
grep -q 'CostQuoteUnavailable' "$WORK/priced.err"
"$BIN" inspect --state "$WORK/priced-state" > "$WORK/priced.snapshot"
priced_turn="$(python3 - "$WORK/priced.snapshot" <<'PY'
import json, sys
s = json.load(open(sys.argv[1]))
assert s['config']['config']['limits']['max_cost_microusd'] == 25
assert s['model_attempts'] == []
assert s['unfinished_turn']['budget']['reserved_cost_microusd'] == 0
print(s['unfinished_turn']['id'])
PY
)"
if ION_SMOKE_SYNTHETIC_KEY=synthetic "$BIN" resume "${priced_common[@]}" \
    --cost-quote-microusd 26 --turn "$priced_turn" \
    > "$WORK/priced-over.out" 2> "$WORK/priced-over.err"; then
    echo 'FAIL: over-budget cost quote reached provider dispatch' >&2; exit 1
fi
grep -q 'MonetaryCapacity' "$WORK/priced-over.err"
"$BIN" inspect --state "$WORK/priced-state" > "$WORK/priced-after.snapshot"
cmp "$WORK/priced.snapshot" "$WORK/priced-after.snapshot"
if ION_SMOKE_SYNTHETIC_KEY=synthetic "$BIN" run "${priced_common[@]}" \
    --model gpt-test --model-input-limit 8192 --model-output-limit 2048 \
    --max-cost-microusd 26 --request-key priced-key 'synthetic priced request' \
    > "$WORK/priced-changed.out" 2> "$WORK/priced-changed.err"; then
    echo 'FAIL: a resumed request changed its frozen monetary ceiling' >&2; exit 1
fi
grep -q 'monetary ceiling differs' "$WORK/priced-changed.err"
"$BIN" inspect --state "$WORK/priced-state" > "$WORK/priced-after-changed.snapshot"
cmp "$WORK/priced.snapshot" "$WORK/priced-after-changed.snapshot"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" resume "${common[@]}" \
    --cost-quote-microusd 1 --turn "$TURN" \
    > "$WORK/uncapped-quote.out" 2> "$WORK/uncapped-quote.err"; then
    echo 'FAIL: uncapped Session accepted a priced host policy' >&2; exit 1
fi
grep -q 'cost quote requires a frozen monetary ceiling' "$WORK/uncapped-quote.err"
"$BIN" inspect --state "$WORK/state" > "$WORK/uncapped-quote.snapshot"
cmp "$WORK/snapshot.json" "$WORK/uncapped-quote.snapshot"

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
assert s['config']['config']['providers'][0]['id'].startswith('anthropic-messages-')
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

# An editable Session must use one explicitly shared, private registry. Fresh
# states share its binding; a different registry incarnation cannot resume it.
mkdir -m 700 "$WORK/shared-registry" "$WORK/other-registry"
mkdir "$WORK/edit-one" "$WORK/edit-two"
edit_common=(--workspace "$WORK/workspace" --endpoint https://api.example.test/v1/chat/completions
             --api-key-env ION_SMOKE_ABSENT_KEY --model gpt-test
             --model-input-limit 8192 --model-output-limit 2048)
if env -u ION_SMOKE_ABSENT_KEY "$BIN" run --state "$WORK/edit-one" --enable-edit \
    "${edit_common[@]}" 'synthetic edit' > "$WORK/no-registry.out" 2> "$WORK/no-registry.err"; then
    echo 'FAIL: edit without a shared registry was admitted' >&2; exit 1
fi
grep -q -- '--enable-edit requires --registry' "$WORK/no-registry.err"
[[ -z "$(find "$WORK/edit-one" -mindepth 1 -maxdepth 1 -print -quit)" ]]
for state in edit-one edit-two; do
    if env -u ION_SMOKE_ABSENT_KEY "$BIN" run --state "$WORK/$state" \
        --registry "$WORK/shared-registry" --enable-edit "${edit_common[@]}" \
        'synthetic edit' > "$WORK/$state.out" 2> "$WORK/$state.err"; then
        echo 'FAIL: missing credentials allowed editable dispatch' >&2; exit 1
    fi
    grep -q 'MissingCredentials' "$WORK/$state.err"
    "$BIN" inspect --state "$WORK/$state" > "$WORK/$state.snapshot"
done
python3 - "$WORK/edit-one.snapshot" "$WORK/edit-two.snapshot" "$WORK/shared-registry/staging" <<'PY'
import json, os, sys
first, second = (json.load(open(path)) for path in sys.argv[1:3])
for snapshot in (first, second):
    config = snapshot['config']['config']
    assert config['authority']['workspace_mutation']
    assert config['initial_tools'] == ['read', 'edit']
    assert [tool['id'] for tool in config['tools']] == ['read', 'edit']
    assert snapshot['tool_attempts'] == []
assert first['config']['config']['workspace'] == second['config']['config']['workspace']
assert os.stat(sys.argv[3]).st_mode & 0o077 == 0
PY
edit_turn="$(python3 - "$WORK/edit-one.snapshot" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))['unfinished_turn']['id'])
PY
)"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" resume --state "$WORK/edit-one" \
    --registry "$WORK/other-registry" --enable-edit \
    --workspace "$WORK/workspace" --endpoint https://api.example.test/v1/chat/completions \
    --api-key-env ION_SMOKE_ABSENT_KEY --turn "$edit_turn" \
    > "$WORK/wrong-registry.out" 2> "$WORK/wrong-registry.err"; then
    echo 'FAIL: a different registry resumed an editable Session' >&2; exit 1
fi
grep -q 'registry incarnation differs' "$WORK/wrong-registry.err"
if env -u ION_SMOKE_ABSENT_KEY "$BIN" resume --state "$WORK/edit-one" \
    --registry "$WORK/shared-registry" \
    --workspace "$WORK/workspace" --endpoint https://api.example.test/v1/chat/completions \
    --api-key-env ION_SMOKE_ABSENT_KEY --turn "$edit_turn" \
    > "$WORK/no-edit.out" 2> "$WORK/no-edit.err"; then
    echo 'FAIL: editable Session resumed without explicit edit enablement' >&2; exit 1
fi
grep -q -- '--enable-edit and host tools must match' "$WORK/no-edit.err"
"$BIN" inspect --state "$WORK/edit-one" > "$WORK/edit-after-refusal.snapshot"
cmp "$WORK/edit-one.snapshot" "$WORK/edit-after-refusal.snapshot"
echo 'headless offline smoke passed (not live-provider or terminal qualification)'
