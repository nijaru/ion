from pathlib import Path

# Normalize only formatting introduced by rustfmt so the semantic patch can
# keep exact, reviewable anchors against the previously validated source.
p = Path("crates/ion-core/src/delegate.rs")
text = p.read_text()
old = '''                            let handle =
                                child_handle_for_session(session_id, self.children.parent_id);
'''
new = '''                            let handle = child_handle_for_session(session_id, self.children.parent_id);
'''
count = text.count(old)
if count < 3:
    raise SystemExit(f"expected at least 3 formatted child-handle anchors, found {count}")
p.write_text(text.replace(old, new))
