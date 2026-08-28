from pathlib import Path

path = Path(".github/runtime_tree_lane_cache_patch.py")
text = path.read_text()

helper_anchor = '''def regex_once(path: str, pattern: str, replacement: str, label: str) -> None:\n'''
helper = '''def replace_first(path: str, old: str, new: str, label: str) -> None:\n    p = Path(path)\n    text = p.read_text()\n    if old not in text:\n        raise SystemExit(f"{label}: expected at least 1 match")\n    p.write_text(text.replace(old, new, 1))\n\n\n'''
if "def replace_first(" not in text:
    if helper_anchor not in text:
        raise SystemExit("replace_first insertion anchor missing")
    text = text.replace(helper_anchor, helper + helper_anchor, 1)

old = '''replace_once(\n    path,\n    \'\'\'        if let Some(pending) = &self.pending_next_run {\n\'\'\',\n    \'\'\'        if let Some(pending) = self.main_pending_next_run() {\n\'\'\',\n    "submit pending lookup",\n)\n'''
new = old.replace("replace_once(", "replace_first(", 1)
if old not in text and new not in text:
    raise SystemExit("submit pending lookup driver block missing")
text = text.replace(old, new, 1)
path.write_text(text)
