#!/usr/bin/env python3
"""Configure release installers for this fork before publishing them."""

from __future__ import annotations

import argparse
import re
from pathlib import Path


REPOSITORY_PATTERN = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")


def replace_regex_once(text: str, pattern: str, replacement: str, path: Path) -> str:
    text, count = re.subn(pattern, replacement, text, flags=re.MULTILINE)
    if count != 1:
        raise RuntimeError(f"expected exactly one pattern {pattern!r} in {path}, found {count}")
    return text


def configure_shell_installer(path: Path, repository: str) -> None:
    text = path.read_text(encoding="utf-8")
    text = replace_regex_once(
        text,
        r'^GITHUB_REPOSITORY="\$\{CODEX_GITHUB_REPOSITORY:-[^}]+\}"$',
        f'GITHUB_REPOSITORY="${{CODEX_GITHUB_REPOSITORY:-{repository}}}"',
        path,
    )
    text = replace_regex_once(
        text,
        r'^GITHUB_TAG_PREFIX="\$\{CODEX_GITHUB_TAG_PREFIX:-[^}]+\}"$',
        'GITHUB_TAG_PREFIX="${CODEX_GITHUB_TAG_PREFIX:-codex-web-v}"',
        path,
    )
    path.write_text(text, encoding="utf-8")


def configure_powershell_installer(path: Path, repository: str) -> None:
    text = path.read_text(encoding="utf-8")
    text = replace_regex_once(
        text,
        r'^\$GitHubRepository = if .*$',
        f'$GitHubRepository = if ([string]::IsNullOrWhiteSpace($env:CODEX_GITHUB_REPOSITORY)) {{ "{repository}" }} else {{ $env:CODEX_GITHUB_REPOSITORY }}',
        path,
    )
    text = replace_regex_once(
        text,
        r'^\$GitHubTagPrefix = if .*$',
        '$GitHubTagPrefix = if ([string]::IsNullOrWhiteSpace($env:CODEX_GITHUB_TAG_PREFIX)) { "codex-web-v" } else { $env:CODEX_GITHUB_TAG_PREFIX }',
        path,
    )
    path.write_text(text, encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--shell-installer", required=True, type=Path)
    parser.add_argument("--powershell-installer", required=True, type=Path)
    args = parser.parse_args()

    if not REPOSITORY_PATTERN.fullmatch(args.repository):
        parser.error("--repository must have the form owner/repository")
    if not args.shell_installer.is_file() or not args.powershell_installer.is_file():
        parser.error("both installer files must already exist")

    configure_shell_installer(args.shell_installer, args.repository)
    configure_powershell_installer(args.powershell_installer, args.repository)
    print(f"Configured Web2Codex installers for {args.repository} and codex-web-v releases")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
