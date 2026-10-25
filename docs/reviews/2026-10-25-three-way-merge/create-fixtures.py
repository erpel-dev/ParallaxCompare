#!/usr/bin/env python3
"""Create local fixtures for M01–M09 without changing an existing repository."""
import argparse
import json
import os
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True,
                        help="A new or empty directory; about 45 MB will be written")
    args = parser.parse_args()
    root = args.directory.expanduser().resolve()
    if root.exists() and (not root.is_dir() or any(root.iterdir())):
        parser.error("--directory must be a new or empty directory")
    root.mkdir(parents=True, exist_ok=True)
    manifest = {}

    def triple(name, base, local, remote, ext="txt"):
        directory = root / name
        directory.mkdir()
        paths = {}
        for side, text in [("base", base), ("local", local), ("remote", remote)]:
            path = directory / f"{side}.{ext}"
            path.write_bytes(text.encode("utf-8"))
            paths[side] = str(path)
        paths["result"] = str(directory / f"result.{ext}")
        manifest[name] = paths
        return directory

    directory = triple("M01-external-save", "one\ntwo\nthree\nfour\nfive\n",
                       "ONE\ntwo\nthree\nfour\nfive\n", "one\ntwo\nthree\nfour\nFIVE\n")
    (directory / "result.txt").write_text("original output\n", encoding="utf-8")
    triple("M04-empty-result", "delete me\n", "", "")
    prefix = "common prefix " * 14
    triple("M05-wide-lines", prefix + "BASE\n", prefix + "LOCAL\n", prefix + "REMOTE\n")
    triple("M06-mergiraf", "fn main() {\n    call(1, 2);\n}\n",
           "fn main() {\n    call(3, 2);\n}\n", "fn main() {\n    call(1, 4);\n}\n", "rs")

    for count in [500, 1000, 2000, 4000]:
        directory = root / f"M07-{count}-conflicts"
        directory.mkdir()
        paths = {}
        for side, label in [("base", "base"), ("local", "local"), ("remote", "remote")]:
            path = directory / f"{side}.txt"
            with path.open("w", encoding="utf-8", newline="") as stream:
                for i in range(count):
                    stream.write(f"{label} value {i}\n")
                    for j in range(6):
                        stream.write(f"context {i}-{j}\n")
            paths[side] = str(path)
        paths["result"] = str(directory / "result.txt")
        manifest[directory.name] = paths

    directory = root / "M08-million-identical"
    directory.mkdir()
    paths = {}
    for side in ["base", "local", "remote"]:
        path = directory / f"{side}.txt"
        with path.open("w", encoding="utf-8", newline="") as stream:
            for start in range(0, 1_000_000, 10_000):
                stream.write("".join(f"entry {i:07d}\n" for i in range(start, start + 10_000)))
        paths[side] = str(path)
    paths["result"] = str(directory / "result.txt")
    manifest[directory.name] = paths

    def partial(field, argument):
        return ("struct C {\n    x: u8,\n" + (f"    {field}: u8,\n" if field else "")
                + "}\nfn f() {\n    g(" + str(argument) + ")\n}\n")

    triple("M09-partial-syntax", partial("", 1), partial("local", 2), partial("remote", 3), "rs")

    # Directly construct unmerged index stages: no commits or identity changes.
    # Linux/macOS are needed to reproduce symlink and executable-bit semantics.
    repo = root / "M02-M03-mode-conflicts"
    repo.mkdir()

    def git(*arguments, data=None):
        return subprocess.run(["git", "-C", str(repo), *arguments], input=data,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout

    git("init", "--quiet", "--initial-branch=main")
    records = []
    for path, modes, texts in [
        ("link", ["120000"] * 3, ["base-target", "local-target", "remote-target"]),
        ("script.sh", ["100755", "100644", "100755"],
         ["header\nbase\nfooter\n", "header\nlocal\nfooter\n", "header\nremote\nfooter\n"]),
    ]:
        for stage, (mode, text) in enumerate(zip(modes, texts), start=1):
            oid = git("hash-object", "-w", "--stdin", data=text.encode()).decode().strip()
            records.append(f"{mode} {oid} {stage}\t{path}\n")
    git("update-index", "--index-info", data="".join(records).encode())
    (repo / "script.sh").write_text("header\nlocal\nfooter\n", encoding="utf-8")
    (repo / "script.sh").chmod(0o644)
    if os.name != "nt":
        (repo / "link").symlink_to("local-target")
    else:
        (repo / "link").write_text("local-target", encoding="utf-8")
    manifest["M02-M03-mode-conflicts"] = {"repository": str(repo)}
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(manifest, indent=2))


if __name__ == "__main__":
    main()
