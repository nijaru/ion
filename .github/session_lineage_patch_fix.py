from pathlib import Path

p = Path('.github/session_lineage_patch.py')
text = p.read_text()
old = '''# Separate-session child handles also name control lineage explicitly.\np = Path("crates/ion-core/src/delegate.rs")\ntext = p.read_text()\ntext = text.replace("parent_session_id", "control_parent_session_id")\n\n# Compute fork history'''
new = '''# Separate-session child handles were renamed by the core-wide pass above.\np = Path("crates/ion-core/src/delegate.rs")\ntext = p.read_text()\n\n# Compute fork history'''
if old not in text:
    raise SystemExit('duplicate delegate lineage rename anchor missing')
p.write_text(text.replace(old, new, 1))
