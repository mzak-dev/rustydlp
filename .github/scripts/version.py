#!/usr/bin/env python3
"""Version arithmetic and version stamping for the release workflows.

Every release path needs the same two things: work out which version to cut,
and write it into the three files that carry one. They each used to do it with
their own inline copy of the same regexes, which is how a fix to the way
Cargo.lock is rewritten lands in one path and silently misses another. This is
that logic once.

    version.py resolve [--version V | --bump patch|minor|major | --preview-of N]
    version.py write VERSION [--manifest]

`resolve` prints `current=`, `version=` and `tag=` in the `key=value` shape a
step appends to $GITHUB_OUTPUT. `write` edits the files in place.
"""

import argparse
import json
import re
import sys
import tomllib

CARGO_TOML = "Cargo.toml"
CARGO_LOCK = "Cargo.lock"
MANIFEST = ".release-please-manifest.json"
CRATE = "rustydlp"

# Semver, minus build metadata: a `+` is legal semver but Cargo tags and git
# tags both handle it badly, so it is rejected rather than half-supported.
SEMVER = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?$"
)


def die(message):
    sys.exit(f"::error::{message}")


def read(path):
    # Universal newlines on the way in, LF on the way out: the preview build
    # runs these edits on a Windows runner, where the checkout may well be
    # CRLF, and a half-converted file is worse than a consistent one.
    with open(path, encoding="utf-8") as f:
        return f.read()


def write_text(path, text):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


def current_version():
    with open(CARGO_TOML, "rb") as f:
        return tomllib.load(f)["package"]["version"]


def core(version):
    """The X.Y.Z of a version, dropping any prerelease or build metadata."""
    return version.split("-", 1)[0].split("+", 1)[0]


def bump(version, part):
    """Bump a version the way `cargo set-version --bump` does.

    Bumping off a prerelease drops the prerelease part: 1.2.3-rc.1 + patch is
    1.2.4, not 1.2.3.
    """
    major, minor, patch = (int(p) for p in core(version).split("."))
    if part == "major":
        return f"{major + 1}.0.0"
    if part == "minor":
        return f"{major}.{minor + 1}.0"
    return f"{major}.{minor}.{patch + 1}"


def cmd_resolve(args):
    current = current_version()

    if args.preview_of is not None:
        # A preview is "newer than the last release, but not itself one", and
        # the patch bump is what makes it sort that way. `0.5.0-preview.18`
        # would order *before* 0.5.0, which is backwards -- the branch holds
        # work that comes after it, not a run-up to it.
        version = f"{bump(current, 'patch')}-preview.{args.preview_of}"
    elif args.version and args.version.strip():
        requested = args.version.strip()
        if requested.startswith("v"):
            die(f"version must not start with 'v': {requested}")
        if not SEMVER.match(requested):
            die(f"not a semver version: {requested}")
        version = requested
    else:
        version = bump(current, args.bump)

    print(f"current={current}")
    print(f"version={version}")
    print(f"tag=v{version}")


def cmd_write(args):
    version = args.version
    if not SEMVER.match(version):
        die(f"not a semver version: {version}")

    # Only the [package] version: the first `version = ` at the top level of
    # Cargo.toml, before any dependency stanza sets one of its own.
    toml, n = re.subn(
        r'(?m)\A(.*?^version = )"[^"]*"',
        rf'\g<1>"{version}"',
        read(CARGO_TOML),
        count=1,
        flags=re.S,
    )
    if n != 1:
        die(f"no [package] version found in {CARGO_TOML}")
    write_text(CARGO_TOML, toml)

    # The lockfile carries the workspace member's own version too, and a stale
    # one makes every later `--locked` build fail.
    lock, n = re.subn(
        rf'(?m)^(\[\[package\]\]\nname = "{CRATE}"\nversion = )"[^"]*"',
        rf'\g<1>"{version}"',
        read(CARGO_LOCK),
        count=1,
    )
    if n != 1:
        die(f"no {CRATE} package entry found in {CARGO_LOCK}")
    write_text(CARGO_LOCK, lock)

    # Only the paths that actually land a release touch the manifest: it is
    # what release-please reads as "the last release", so a preview build
    # writing to it would make the next automated release bump from a version
    # that was never published.
    if args.manifest:
        manifest = json.loads(read(MANIFEST))
        manifest["."] = version
        write_text(MANIFEST, json.dumps(manifest, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    resolve = sub.add_parser("resolve", help="work out the version to cut")
    resolve.add_argument("--version", default="", help="an exact version, without a leading 'v'")
    resolve.add_argument("--bump", default="patch", choices=["patch", "minor", "major"])
    resolve.add_argument("--preview-of", metavar="PR", help="build a preview version for this PR")
    resolve.set_defaults(func=cmd_resolve)

    write = sub.add_parser("write", help="stamp a version into the files that carry one")
    write.add_argument("version")
    write.add_argument("--manifest", action="store_true", help="also move .release-please-manifest.json")
    write.set_defaults(func=cmd_write)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
