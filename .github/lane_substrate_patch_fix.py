from pathlib import Path

path = Path(".github/lane_substrate_patch.py")
text = path.read_text()
needle = '''# Exact persistence failure routing should include formatting variants.
text = text.replace(
'''
insert = '''# Formatting-insensitive recovery routing: rustfmt often splits `self` from
# the method selector, so rewrite the selector itself after the exact forms.
text = text.replace(".main_active()", ".active(operation_id)")
text = text.replace(".main_live_mut()", ".live_mut(operation_id)")
text = text.replace(".main_live()", ".live(operation_id)")
text = text.replace(".main_resident_id()", ".resident(operation_id).map(|_| operation_id)")

# Exact persistence failure routing should include formatting variants.
text = text.replace(
'''
count = text.count(needle)
if count != 1:
    raise SystemExit(f"recovery rewrite insertion point: expected 1, found {count}")
path.write_text(text.replace(needle, insert, 1))
