"""Small disposable fixtures for E01–E08; no application dependencies.

Create: python3 fixtures.py create /tmp/parallax-review-fixtures
E05 external edit: python3 fixtures.py replace /tmp/parallax-review-fixtures/archive-stale/dirty.zip
The create destination must not exist. Replacement is limited to ZIPs created here.
"""
import argparse
import os
from pathlib import Path
import zipfile


def pack(path, entries):
    with zipfile.ZipFile(path, "w") as archive:
        for name, text in entries.items():
            archive.writestr(name, text)


def create(root):
    root.mkdir(parents=True, exist_ok=False)

    def put(rel, text):
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    for side in ("left", "right"):
        for name in ("a.txt", " leading.txt", "trailing.txt ", "report final.txt", "report,final.txt", '"quote.txt'):
            put(f"literal-only/{side}/{name}", side + "\n")
        for name in ("a,b.txt", "a.txt", "b.txt", "c.txt"):
            put(f"escape-patterns/{side}/{name}", side + "\n")
        for case in ("copy-match-right-only", "copy-empty-subtrees"):
            put(f"{case}/{side}/wrapper/normal.txt", side + " normal\n")
        entries = {"hidden.bin": side + " hidden\n"}
        if side == "right":
            entries["keep.txt"] = "only on right\n"
        pack(root / f"copy-match-right-only/{side}/wrapper/bundle.zip", entries)
        for n in (100, 150):
            prefix = "old" if side == "left" else "new"
            for i in range(n):
                put(f"flat-{n}/{side}/{prefix}{i:04}/index.txt", "identical file\n")
        put(f"cancel-form/{side}/changed.txt", side + " value\n")
    put("copy-empty-subtrees/left/wrapper/hidden/deep/secret.bin", "hidden\n")
    put("archive-stale/left.txt", "old A\n")
    for name in ("dirty.zip", "clean.zip"):
        pack(root / "archive-stale" / name, {"a.txt": "old A\n", "b.txt": "old B\n"})
    print(root)


def replace(path, preserve_mtime):
    if path.name not in ("dirty.zip", "clean.zip") or path.parent.name != "archive-stale":
        raise SystemExit("Expected an archive-stale/dirty.zip or archive-stale/clean.zip fixture")
    stamp = path.stat()
    temp = path.with_suffix(".new.zip")
    pack(temp, {"a.txt": "EXT A\n", "b.txt": "EXT B\n"})
    os.replace(temp, path)
    if preserve_mtime:
        os.utime(path, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
    print("Replaced", path)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("create", "replace"))
    parser.add_argument("path", type=Path)
    parser.add_argument("--preserve-mtime", action="store_true")
    args = parser.parse_args()
    if args.action == "create":
        create(args.path)
    else:
        replace(args.path, args.preserve_mtime)
