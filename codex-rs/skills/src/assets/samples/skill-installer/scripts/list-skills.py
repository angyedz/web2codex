#!/usr/bin/env python3
"""List skills from a GitHub repo path."""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error

from github_utils import github_api_contents_url, github_request

DEFAULT_REF = "main"


class ListError(Exception):
    pass


class Args(argparse.Namespace):
    repo: str | None
    path: str | None
    ref: str
    format: str
    installed: bool


def _request(url: str) -> bytes:
    return github_request(url, "web2codex-skill-list")


def _codex_home() -> str:
    return os.environ.get(
        "WEB2CODEX_HOME",
        os.environ.get("CODEX_HOME", os.path.expanduser("~/.web2codex")),
    )


def _installed_skills() -> set[str]:
    return set(_list_installed_skills())


def _list_installed_skills() -> list[str]:
    root = os.path.join(_codex_home(), "skills")
    if not os.path.isdir(root):
        return []
    skills = []
    for name in os.listdir(root):
        path = os.path.join(root, name)
        if name == ".system" or not os.path.isdir(path):
            continue
        if os.path.isfile(os.path.join(path, "SKILL.md")):
            skills.append(name)
    return sorted(skills)


def _list_skills(repo: str, path: str, ref: str) -> list[str]:
    api_url = github_api_contents_url(repo, path, ref)
    try:
        payload = _request(api_url)
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            raise ListError(
                f"Skills path not found: https://github.com/{repo}/tree/{ref}/{path}"
            ) from exc
        raise ListError(f"Failed to fetch skills: HTTP {exc.code}") from exc
    data = json.loads(payload.decode("utf-8"))
    if not isinstance(data, list):
        raise ListError("Unexpected skills listing response.")
    skills = [item["name"] for item in data if item.get("type") == "dir"]
    return sorted(skills)


def _parse_args(argv: list[str]) -> Args:
    parser = argparse.ArgumentParser(
        description="List installed skills or skills in an explicit GitHub repository path."
    )
    parser.add_argument("--repo")
    parser.add_argument(
        "--path",
        help="Repository path containing skill directories (required with --repo)",
    )
    parser.add_argument("--ref", default=DEFAULT_REF)
    parser.add_argument(
        "--installed",
        action="store_true",
        help="List profile-local host skills without network access",
    )
    parser.add_argument(
        "--format",
        choices=["text", "json"],
        default="text",
        help="Output format",
    )
    return parser.parse_args(argv, namespace=Args())


def main(argv: list[str]) -> int:
    args = _parse_args(argv)
    try:
        if args.installed:
            if args.repo or args.path:
                raise ListError("--installed cannot be combined with --repo or --path.")
            skills = _list_installed_skills()
            if args.format == "json":
                print(json.dumps([{"name": name, "installed": True} for name in skills]))
            elif skills:
                for idx, name in enumerate(skills, start=1):
                    print(f"{idx}. {name} (installed)")
            else:
                print("No profile-local host skills were found in this Web2Codex profile.")
            return 0
        if not args.repo or not args.path:
            raise ListError("Provide both --repo and --path for remote listing, or use --installed.")
        skills = _list_skills(args.repo, args.path, args.ref)
        installed = _installed_skills()
        if args.format == "json":
            payload = [
                {"name": name, "installed": name in installed} for name in skills
            ]
            print(json.dumps(payload))
        else:
            for idx, name in enumerate(skills, start=1):
                suffix = " (already installed)" if name in installed else ""
                print(f"{idx}. {name}{suffix}")
        return 0
    except ListError as exc:
        print(f"Error: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
