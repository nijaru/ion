"""Independent operator-side verification of committed Session capture bytes."""
import hashlib
import json
import sqlite3
import uuid
from pathlib import Path

# The guest intentionally has no Node/browser modules; decode using ordinary JS.
BASE64_JS = """
const base64Bytes = encoded => {
  const alphabet='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  let accumulator=0,bits=0; const bytes=[];
  for(const char of encoded) {
    if(char==='=') break;
    const value=alphabet.indexOf(char); if(value<0) throw new Error('invalid base64');
    accumulator=(accumulator<<6)|value; bits+=6;
    if(bits>=8) {bits-=8; bytes.push((accumulator>>bits)&255);}
  }
  return bytes;
};
const utf8 = bytes => decodeURIComponent(bytes.map(byte=>'%'+byte.toString(16).padStart(2,'0')).join(''));
"""


def artifact_bytes(root, reference):
    """Verify receipts, contiguous chunks and both checksums without Ion helpers."""
    for path in Path(root).rglob("*.sqlite"):
        with sqlite3.connect(f"{path.as_uri()}?mode=ro", uri=True) as db:
            if not db.execute("SELECT 1 FROM sqlite_master WHERE name='artifacts'").fetchone():
                continue
            row = db.execute("SELECT receipt FROM artifacts WHERE id=?", (reference["id"],)).fetchone()
            if row is None:
                continue
            assert json.loads(row[0]) == reference, (path, reference)
            data = bytearray()
            for offset, body, checksum in db.execute(
                "SELECT byte_offset,body,checksum FROM artifact_chunks WHERE artifact=? ORDER BY byte_offset",
                (reference["id"],),
            ):
                assert offset == len(data) and 1 <= len(body) <= 262144
                bound = uuid.UUID(reference["id"]).bytes + offset.to_bytes(8, "big") + body
                assert hashlib.sha256(bound).hexdigest() == checksum
                data.extend(body)
            assert len(data) == reference["bytes"]
            assert hashlib.sha256(data).hexdigest() == reference["sha256"]
            assert path.stat().st_mode & 0o777 == 0o600
            return bytes(data)
    raise AssertionError(f"no committed artifact: {reference['id']}")
