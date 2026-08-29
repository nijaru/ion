from pathlib import Path

p = Path("crates/ion-core/src/tool/mod.rs")
text = p.read_text()
old = '''pub(crate) async fn reconciliation_evidence(
    cwd: &Path,
    name: &str,
    arguments: &Value,
) -> Result<Value, String> {
'''
new = '''#[cfg(test)]
pub(crate) async fn reconciliation_evidence(
    cwd: &Path,
    name: &str,
    arguments: &Value,
) -> Result<Value, String> {
'''
if old not in text:
    raise SystemExit("reconciliation helper anchor missing")
p.write_text(text.replace(old, new, 1))
