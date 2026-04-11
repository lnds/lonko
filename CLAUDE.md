# Lonko — Claude instructions

## Before you start

Read [`ARCHITECTURE.md`](./ARCHITECTURE.md) first. It describes the crate layout, the event loop, the tmux integration, and the hook protocol. Skipping it will lead to changes that fight the existing design.

## Language

**All code, comments, doc comments, commit messages, PR titles, PR descriptions, and review comments must be written in English.** No exceptions. This project is shared and English is the common language for contributors.

All existing comments are in English. Do not introduce Spanish comments.

## Commits

This repo follows [Conventional Commits](https://www.conventionalcommits.org/). Use the standard prefixes:

- `feat:` — new user-facing feature
- `fix:` — bug fix
- `refactor:` — internal change with no behavior difference
- `chore:` — tooling, config, deps, etc.
- `docs:` — documentation only
- `test:` — tests only
- `perf:` — performance improvement

Scope is optional but welcome when it clarifies the area (e.g., `feat(tmux): ...`). Keep the subject under ~70 characters; put details in the body.

## Version bumping

Version bumps are handled by [Commitizen](https://commitizen-tools.github.io/commitizen/) (the Python tool, installed via `pip install commitizen` or `pipx install commitizen`). **Never edit version numbers by hand.**

To bump:

```sh
cz bump -ch --yes
```

- `-ch` updates the changelog.
- `--yes` is required because the interactive prompt crashes under the Claude Code harness.

Commitizen reads Conventional Commit messages to decide the bump level (patch/minor/major), so well-formed commits matter.
