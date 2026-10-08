#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0 OR MIT
# SPDX-FileCopyrightText: 2026 Denis Yermakou <connect@axonos.org>
"""Fail if a dependency tag no longer points where the lockfile says it does.

`cargo build --locked` guarantees the *lockfile* is unchanged. It does not
notice when a tag on the remote is moved to a different commit: cargo fetches
the recorded revision and builds happily, so a tag repointed by whoever owns
that repository is invisible to every other check in this pipeline.

That is the supply-chain event worth catching. A tag is a mutable pointer with
an immutable-sounding name, and the whole organ stack is assembled from them.

Read-only, standard library only, no token needed for public repositories.
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

LOCK = Path("Cargo.lock")
# source = "git+https://github.com/OWNER/REPO?tag=vX.Y.Z#<40 hex>"
SRC = re.compile(
    r'source = "git\+(?P<url>[^?"]+)\?tag=(?P<tag>[^#"]+)#(?P<rev>[0-9a-f]{40})"'
)


def main() -> int:
    if not LOCK.exists():
        print("::error::Cargo.lock is absent — this crate must commit it")
        return 2

    pins = SRC.findall(LOCK.read_text(encoding="utf-8"))
    if not pins:
        print("::error::no git-pinned dependencies found in Cargo.lock")
        return 2

    bad = 0
    for url, tag, rev in pins:
        name = url.rstrip("/").rsplit("/", 1)[-1]
        try:
            out = subprocess.run(
                ["git", "ls-remote", url + ".git", f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"],
                capture_output=True, text=True, timeout=60, check=True,
            ).stdout
        except Exception as e:  # noqa: BLE001
            print(f"::error::{name}: cannot reach {url} ({e})")
            bad += 1
            continue

        refs = dict(
            (line.split("\t")[1], line.split("\t")[0])
            for line in out.strip().splitlines() if "\t" in line
        )
        # An annotated tag resolves through its peeled ref; a lightweight one
        # does not have one. Prefer the peeled value, which is the commit.
        remote = refs.get(f"refs/tags/{tag}^{{}}") or refs.get(f"refs/tags/{tag}")
        if remote is None:
            print(f"::error::{name}: tag {tag} no longer exists on the remote")
            bad += 1
        elif remote != rev:
            print(f"::error::{name}: tag {tag} MOVED")
            print(f"           locked  {rev}")
            print(f"           remote  {remote}")
            print("           A tag was repointed after this lockfile was written.")
            bad += 1
        else:
            print(f"  ok  {name} {tag} -> {rev[:12]}")

    if bad:
        print(f"::error::{bad} pinned dependency tag(s) do not match the lockfile")
        return 1
    print(f"all {len(pins)} pinned dependency tags still point where the lockfile says")
    return 0


if __name__ == "__main__":
    sys.exit(main())
