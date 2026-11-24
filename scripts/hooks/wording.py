#!/usr/bin/env python3
"""Wording check for the repository: rejects terms on the project's list and local home paths.

    wording.py --staged          added lines and paths of the staged changes (pre-commit)
    wording.py --message FILE    a commit message (commit-msg); comment lines and the scissors part are skipped
    wording.py --tree            every tracked file and path of the working tree (CI)
    wording.py --log RANGE       the messages of the commits in RANGE, e.g. origin/main..HEAD (pre-push, CI)

Text is lowercased and split into words on anything that is not a letter or a digit, so "Foo-Bar" is the two words
"foo bar". Each word and each pair of adjacent words is compared with the list below. The list holds SHA-256 digests
of the lowercase terms (one word, or two words joined by one space) so that it does not trip the check itself; to add
a term, append `printf %s 'term' | sha256sum`. A home path is a "/home/<name>" or "/Users/<name>" path.

Files written by tools or copied verbatim (Cargo.lock, the licence texts) are checked for home paths only: their
boilerplate is not ours to word.

Exit status: 0 clean, 1 findings (printed as `where: text`), 2 usage error.
"""

import hashlib
import re
import subprocess
import sys

TERMS = frozenset(
    """
c857d09db23e6822e3600bc06ad8d58f92ed62bc8efd81c753f77048662cb97d
c70eca6b0f88f44d81a41311647e50fda1ac454ec04ffd442b0eb4743a993131
487b91042c7cf27a19e23ea8699f5f354b1a0c3af9e418138dc6150d830f970d
de2187af3fb011708ca4ff6d21e4845236d6a6d25e7e90531b26a7923d16c73f
ee662f0a4316976d226a3fafe98060cd4995718d09c62f78dd1d359399a75528
e748f8750672ca8b04b242bef501be5fab814415306bbcf46ca52e70a9e8c9e6
cdcbb68a3444fdd629ab0af770af3fc99e4d5dde43b576d5f855691e155283ec
8316b399af66208937f9fe28d70a160fa0d26060ab53df53cb5ab55c10eb4cfe
1655c98d0af594ad83b151da1676f5063809b8b6fc4de61ff29907b94fcd79b5
bf24385098410391a81d92b2de72d3a2946d24f42ee387e51004a868281a2408
fb9afa84739c3239a0f5a57153fa5e9e2367f1adb4d2ef9a2c47f2cba31a74b0
e12ce8285efc67c6d93d3a122e2589ed95089bcbb775ba5634d94e2b8385db07
c9ad8f2cc1294afa0ef22fc2c019ff7243cdd272b2147ddca3f34c5036b05768
60965168ce762e949600281ba6d01fee136e5b6e8257b1f216f9025ed324474c
7d3194f79e645c42e4396dda38be04766810ec6a00d00aced3ffc2a0a1f1a9ef
3ea125d0bff386e6754b3782b300016fc79a9cf8f8669c0a5c3db64467ddb681
953de24889d67ab24d43eff8b1058d3edca3adfc4bfa98e9403853b446470d65
""".split()
)

WORD = re.compile(r"[a-z0-9]+")
HOME_PATH = re.compile(r"/(?:home|Users)/[A-Za-z0-9._-]+")
MAX_FILE = 5 * 1024 * 1024
# File names (in any directory): lock files are written by cargo, licence texts are copied verbatim.
PATHS_ONLY = frozenset({"Cargo.lock", "LICENSE", "LICENSE-APACHE", "LICENSE-MIT"})


def _digest(s: str) -> str:
    return hashlib.sha256(s.encode()).hexdigest()


def findings(text: str, paths_only: bool = False) -> list[str]:
    """Terms on the list and home paths found in one line of text."""
    hits = []
    words = [] if paths_only else WORD.findall(text.lower())
    for i, w in enumerate(words):
        if _digest(w) in TERMS:
            hits.append(w)
        if i + 1 < len(words) and _digest(f"{w} {words[i + 1]}") in TERMS:
            hits.append(f"{w} {words[i + 1]}")
    hits.extend(HOME_PATH.findall(text))
    return hits


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True, errors="replace").stdout


def check_lines(where: str, lines, out: list[str]) -> None:
    paths_only = where.rsplit("/", 1)[-1] in PATHS_ONLY
    for n, line in lines:
        hits = findings(line, paths_only)
        if hits:
            out.append(f"{where}:{n}: {', '.join(sorted(set(hits)))}")


def staged(out: list[str]) -> None:
    for path in git("diff", "--cached", "--name-only", "--diff-filter=ACMR", "-z").split("\0"):
        if path:
            check_lines(f"path {path}", [(0, path)], out)
    path, n = None, 0
    for line in git("diff", "--cached", "--no-color", "--no-ext-diff", "-U0", "--diff-filter=ACMR").splitlines():
        if line.startswith("+++ "):
            path = line[6:] if line.startswith("+++ b/") else line[4:]
        elif line.startswith("@@"):
            m = re.search(r"\+(\d+)", line)
            n = int(m.group(1)) if m else 0
        elif line.startswith("+") and path is not None:
            check_lines(path, [(n, line[1:])], out)
            n += 1


def message_lines(text: str):
    for n, line in enumerate(text.splitlines(), 1):
        if line.startswith("# ------------------------ >8 ------------------------"):
            break
        if not line.startswith("#"):
            yield n, line


def tree(out: list[str]) -> None:
    for path in git("ls-files", "-z").split("\0"):
        if not path:
            continue
        check_lines(f"path {path}", [(0, path)], out)
        try:
            with open(path, "rb") as f:
                data = f.read(MAX_FILE + 1)
        except (FileNotFoundError, IsADirectoryError):
            continue
        if len(data) > MAX_FILE or b"\0" in data[:8192]:
            continue
        check_lines(path, enumerate(data.decode("utf-8", "replace").splitlines(), 1), out)


def log(rng: str, out: list[str]) -> None:
    for rec in git("log", "--format=%H%x00%B%x01", rng).split("\x01"):
        rec = rec.strip("\n")
        if rec:
            sha, _, body = rec.partition("\0")
            check_lines(f"commit {sha[:12]}", message_lines(body), out)


def main(argv: list[str]) -> int:
    out: list[str] = []
    if argv == ["--staged"]:
        staged(out)
    elif len(argv) == 2 and argv[0] == "--message":
        with open(argv[1], encoding="utf-8", errors="replace") as f:
            check_lines("commit message", message_lines(f.read()), out)
    elif argv == ["--tree"]:
        tree(out)
    elif len(argv) == 2 and argv[0] == "--log":
        log(argv[1], out)
    else:
        print(__doc__, file=sys.stderr)
        return 2
    for line in out:
        print(line)
    return 1 if out else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
