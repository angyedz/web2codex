---
name: skill-installer
description: Install compatible skills into the Web2Codex profile from a user-selected GitHub repository or local directory. Use when the user asks to find, install, or update a reusable skill.
metadata:
  short-description: Install skills from a chosen source
---

# Skill Installer

Manage reusable skills in the current Web2Codex profile. There is no default remote catalog. Never choose a remote source on the user's behalf.

## Workflow

- If the user asks which skills are available, use the model-visible skills catalog already included for this turn. Use `scripts/list-skills.py --installed` only when they specifically ask which host skills are installed in this profile.
- List remote skills only when the user names or approves a repository and repository path. Pass both `--repo <owner>/<repo>` and `--path <path>`.
- Install from GitHub only after the user identifies the repository and skill path. For private repositories, use existing Git credentials or optional `GITHUB_TOKEN`/`GH_TOKEN`.
- For a local skill directory, inspect its `SKILL.md`, then copy the complete directory into `$CODEX_HOME/skills/<skill-name>` while preserving its relative resources.
- Before copying, check whether the destination already exists. Do not overwrite an installed skill unless the user asks to update it.
- After installation, explain that the skill is available on the next turn.

## Scripts

Remote operations use the network; local listing does not.

- `scripts/list-skills.py --installed` (lists only profile-local host skills, without network access)
- `scripts/list-skills.py --repo <owner>/<repo> --path <path/to/skills> [--ref <ref>]`
- `scripts/list-skills.py --repo <owner>/<repo> --path <path/to/skills> --format json`
- `scripts/install-skill-from-github.py --repo <owner>/<repo> --path <path/to/skill> [<path/to/skill> ...]`
- `scripts/install-skill-from-github.py --url https://github.com/<owner>/<repo>/tree/<ref>/<path>`

## Storage and safety

- Installations are written to `$CODEX_HOME/skills/<skill-name>`; this harness sets `CODEX_HOME` to its isolated home and defaults it to `~/.web2codex`.
- The installer rejects path traversal, unsafe archive entries, symbolic links, and destinations that already exist.
- Never send credentials to a remote repository. Private GitHub access uses existing Git credentials or the script's supported token environment variables.
