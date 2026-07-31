#!/usr/bin/env python3
"""Fails when ci.yml's warm-cache build and release.yml's build diverge.

The Windows release build restores a cache that ci.yml's `warm-release-cache`
job populated on the default branch. A restored cache is only *usable* when the
compilation it holds matches: cargo unifies features per invocation, so
`cargo build -p a -p b --features a/x` and two separate builds produce
different fingerprints for every dependency `a` and `b` share, and the release
recompiles them with a warm cache sitting right there.

That is not hypothetical, it is how this file came to exist: the release build
was split in two (so the client stops linking OpenSSL it never calls) and the
warm job kept running the single old command for a whole release cycle.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUILD = re.compile(r"cargo build --release --locked[^\n\"]*")


def invocations(workflow: str) -> list[str]:
    """The release-build cargo commands of a workflow, target-independent.

    The target differs by design (the warm job pins Windows, the release job
    takes it from the matrix), so it is normalized away; everything else,
    package selection, feature flags and order, has to match.
    """
    text = (ROOT / ".github" / "workflows" / workflow).read_text()
    out = []
    for line in BUILD.findall(text):
        line = re.sub(r"--target \S+( matrix\.target \}\})?", "--target T", line)
        out.append(" ".join(line.split()))
    return out


def main() -> int:
    warm = invocations("ci.yml")
    release = invocations("release.yml")
    if warm == release:
        print(f"ok: warm cache and release build run the same {len(warm)} invocation(s)")
        return 0
    print("The warm cache job and the release build have diverged.\n")
    print("ci.yml (warm-release-cache):")
    for c in warm or ["(none found)"]:
        print(f"  {c}")
    print("\nrelease.yml:")
    for c in release or ["(none found)"]:
        print(f"  {c}")
    print(
        "\nThey must match, including the split into separate invocations: cargo\n"
        "unifies features per invocation, so a different shape means the release\n"
        "recompiles every shared dependency instead of restoring it."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
