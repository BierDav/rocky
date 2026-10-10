"""Reject missing, extra or altered packaged files before consumption."""

import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
expected = json.loads((root / "SHA256.json").read_text())
actual = {
    str(file.relative_to(root)): hashlib.file_digest(file.open("rb"), "sha256").hexdigest()
    for file in root.rglob("*") if file.is_file() and file != root / "SHA256.json"
}
if actual != expected:
    raise ValueError("iOS inventory does not match the package")
print(f"iOS inventory verified: {len(actual)} files")
