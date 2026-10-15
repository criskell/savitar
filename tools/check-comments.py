#!/usr/bin/env python3
import re
import subprocess
import sys

EXTS = (".rs", ".ts", ".tsx", ".mts", ".cts")
LINE = re.compile(r"(^|[^:/])//")
BLOCK = re.compile(r"/\*")


def tracked():
    try:
        out = subprocess.run(
            ["git", "ls-files", "-z"],
            capture_output=True,
            check=True,
            text=True,
        ).stdout
        return [p for p in out.split("\0") if p.endswith(EXTS)]
    except (subprocess.CalledProcessError, FileNotFoundError):
        from pathlib import Path

        return [str(p) for p in Path(".").rglob("*") if p.suffix in EXTS and "target" not in p.parts]


def strip_strings(line):
    return re.sub(r'"(\\.|[^"\\])*"', '""', line)


def main():
    bad = []
    for path in tracked():
        try:
            with open(path, encoding="utf-8") as handle:
                lines = handle.readlines()
        except OSError:
            continue
        for number, raw in enumerate(lines, start=1):
            line = strip_strings(raw)
            if LINE.search(line) or BLOCK.search(line):
                bad.append(f"{path}:{number}: {raw.strip()}")

    if bad:
        print(f"{len(bad)} comentario(s) encontrado(s); CLAUDE.md proibe comentarios.")
        for entry in bad:
            print(f"  {entry}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
