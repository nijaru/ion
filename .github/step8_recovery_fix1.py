from pathlib import Path

path = Path("crates/ion-core/src/runtime/recovery.rs")
text = path.read_text()
old = "let call = invocation.into_call(operation_id);"
new = "let call = invocation.clone().into_call(operation_id);"
count = text.count(old)
if count != 1:
    raise SystemExit(f"expected one consuming reconciliation call, found {count}")
path.write_text(text.replace(old, new, 1))
print("Step 8 reconciliation invocation move fixed")
