#!/usr/bin/env python3
"""Set the Cargo workspace version in an isolated release build checkout."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path


SEMVER = re.compile(
    r"(?P<major>0|[1-9]\d*)\."
    r"(?P<minor>0|[1-9]\d*)\."
    r"(?P<patch>0|[1-9]\d*)"
    r"(?:-(?P<prerelease>[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)
MANIFEST = Path(__file__).resolve().parents[2] / "codex-rs" / "Cargo.toml"
LOCKFILE = MANIFEST.with_name("Cargo.lock")
PACKAGE_BLOCK = re.compile(r"(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]\n|\Z)")


def main() -> int:
    match = SEMVER.fullmatch(sys.argv[1]) if len(sys.argv) == 2 else None
    if match is None:
        print(
            "usage: set_fork_release_version.py MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]",
            file=sys.stderr,
        )
        return 2

    release_version = sys.argv[1]
    if any(
        int(match.group(component)) > 2**64 - 1
        for component in ("major", "minor", "patch")
    ):
        print("version components must fit in an unsigned 64-bit integer", file=sys.stderr)
        return 2
    prerelease = match.group("prerelease") or ""
    if any(
        part.isdigit() and len(part) > 1 and part.startswith("0")
        for part in prerelease.split(".")
    ):
        print("numeric prerelease identifiers may not have leading zeroes", file=sys.stderr)
        return 2

    text = MANIFEST.read_text(encoding="utf-8")
    workspace = tomllib.loads(text).get("workspace", {})
    old_version = workspace.get("package", {}).get("version")
    if not isinstance(old_version, str):
        raise RuntimeError("[workspace.package].version must be a string")

    section = re.compile(r"(?ms)(^\[workspace\.package\]\s*\n)(.*?)(?=^\[|\Z)")
    match = section.search(text)
    if match is None:
        raise RuntimeError("[workspace.package] was not found")
    body = match.group(2)
    updated, count = re.subn(
        r'(?m)^version\s*=\s*"[^"]+"\s*$',
        f'version = "{release_version}"',
        body,
    )
    if count != 1:
        raise RuntimeError(f"expected one workspace version, found {count}")

    package_names: set[str] = set()
    workspace_root = MANIFEST.parent
    excluded_directories = {
        path.resolve()
        for pattern in workspace.get("exclude", [])
        for path in workspace_root.glob(pattern)
    }
    member_manifests: set[Path] = set()
    for pattern in workspace.get("members", []):
        for directory in workspace_root.glob(pattern):
            if directory.is_dir() and directory.resolve() not in excluded_directories:
                manifest = directory / "Cargo.toml"
                if manifest.is_file():
                    member_manifests.add(manifest)

    for manifest in member_manifests:
        package = tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {})
        package_version = package.get("version")
        if isinstance(package_version, dict) and package_version.get("workspace") is True:
            package_names.add(package["name"])

    lock_text = LOCKFILE.read_text(encoding="utf-8")
    lock_updated_names: set[str] = set()

    def update_lock_package(block: str) -> str:
        package = tomllib.loads(block).get("package", {})
        name = package.get("name")
        if (
            package.get("source") is not None
            or name not in package_names
            or package.get("version") != old_version
        ):
            return block
        replaced, replacements = re.subn(
            r'(?m)^(version\s*=\s*)"[^"]+"\s*$',
            rf'\g<1>"{release_version}"',
            block,
            count=1,
        )
        if replacements != 1:
            raise RuntimeError(f"could not update Cargo.lock package {name!r}")
        lock_updated_names.add(name)
        return replaced

    lock_updated = PACKAGE_BLOCK.sub(update_lock_package, lock_text)
    missing = package_names - lock_updated_names
    if missing:
        raise RuntimeError(
            "Cargo.lock is missing workspace package entries for: "
            + ", ".join(sorted(missing))
        )

    updated_manifest = text[: match.start(2)] + updated + text[match.end(2) :]
    MANIFEST.write_text(updated_manifest, encoding="utf-8")
    LOCKFILE.write_text(lock_updated, encoding="utf-8")
    print(
        f"Set Cargo workspace version to {release_version} and updated "
        f"{len(lock_updated_names)} workspace package entries in Cargo.lock"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
