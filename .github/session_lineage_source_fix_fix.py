from pathlib import Path

p = Path('.github/session_lineage_source_fix.py')
text = p.read_text()
old = 'text = text.replace("parent: self.parent_id,", "control_parent: self.parent_id,")'
new = 'text = text.replace("\\n                parent: self.parent_id,", "\\n                control_parent: self.parent_id,", 1)'
if old not in text:
    raise SystemExit('broad ChildRuntimeConfig replacement anchor missing')
p.write_text(text.replace(old, new, 1))
