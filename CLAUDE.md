# Shepherd — notes for Claude

## Language

All commit messages must be in **English**. No Spanish in commit subjects or bodies.

## Versioning

Use **commitizen** to bump the version:

```
cz bump -ch
```

This updates the version in all `Cargo.toml` files, creates the bump commit, and updates the changelog automatically. Do not edit versions manually or use `sed`.

**Important:** never run `cz bump` without explicit authorization from the user. Always ask first.

The correct workflow after implementing a fix or feature:

1. Build: `cargo build -p shepherd`
2. Commit with a conventional commit message (English)
3. Ask the user if they want to bump the version
4. If yes: `cz bump -ch`, then rebuild and install the binary

## Binary location

The shepherd binary lives **only** at `~/.cargo/bin/shepherd`. The user has `~/bin/shepherd` as a symlink to it, and `~/bin` takes precedence in PATH. Never copy the binary to `/opt/homebrew/bin/shepherd` or any other PATH directory — it causes duplicate binaries that serve stale versions after a rebuild. To install a fresh build, use the install script (builds in release mode):

```
bash install.sh
```

If a running shepherd process is serving an old version after a rebuild, kill it with `pkill -f '^shepherd$'` so the next launch picks up the new binary.
