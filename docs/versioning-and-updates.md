# Versioning and updates

This document owns the version, installation, and update contract for `buzzx`.
It covers both supported installation paths: a prebuilt release and a build from
source. The relay is not a binary distribution source.

## Release status

The repository has no published tag yet. `Cargo.toml` contains the planned
package version (`0.1.0`), but a binary built from `main` is not a `0.1.0`
release. Do not publish `v0.1.0` until the acceptance in
[`eng/release.md`](../eng/release.md) passes.

The first public candidate uses a tag such as `v0.1.0-rc.1`. Publish
`v0.1.0` only after the candidate has passed the same install and live-relay
checks that users depend on.

## Version vocabulary

There is one release version. `Cargo.toml`, the Git tag, the GitHub Release,
archive names, and an exact release build must agree on the SemVer value.

| Build | Displayed version | Channel | Update meaning |
|---|---|---|---|
| Exact stable tag `v0.1.0` | `0.1.0` | `stable` | Compare with the latest stable release |
| Exact prerelease tag `v0.1.0-rc.1` | `0.1.0-rc.1` | `prerelease` | Compare only when prereleases are requested |
| Clean `main` commit | `0.0.0-main.gabcdef123456` | `dev` | Follow `origin/main`, never stable |
| Clean non-main commit | `0.0.0-dev.gabcdef123456` | `dev` | Not eligible for managed source updates |
| Dirty source tree | The same value with `.dirty` appended | `dev` | Warn that the binary is not reproducible |
| Source without Git metadata | `0.0.0-dev.unknown` | `dev` | Show source-install instructions |

`gabcdef123456` is a fixed-length, 12-character short Git object id with a
`g` prefix. These are runtime build identities, not versions edited into
`Cargo.toml` on every commit. A source build and a prebuilt binary are both
valid clients; `install: source` is provenance, not a compatibility failure.

## What the user sees

`buzzx --version` remains a one-line, script-friendly answer:

```text literal
buzzx 0.1.0
```

An untagged `main` build is, for example:

```text literal
buzzx 0.0.0-main.gabcdef123456
```

The TUI About surface carries diagnostic provenance without making the channel
header noisy:

```text literal
buzzx 0.0.0-main.gabcdef123456
channel: dev
source: main @ abcdef123456 (clean)
install: source
target: x86_64-unknown-linux-gnu
relay: buzz.surac.cloud
config: ~/.config/buzzx/config.toml
```

## Installation paths

Choose one path and keep using its update procedure. `buzzx` never silently
changes a source installation into a prebuilt installation.

### Prebuilt release

The release workflow builds Linux, macOS, and Windows artifacts. Installation
and checksum instructions are in [`installation.md`](installation.md). The
installer changes the binary and PATH only; it does not remove configuration,
private keys, relay preferences, or read state.

### Tagged source install (manual)

Use a release tag when the source build must reproduce a published version:

```sh
git clone https://github.com/suraciii/buzzx.git
cd buzzx
git checkout v0.1.0
cargo install --locked --path . --force
```

Tagged source installs remain manual. To select another release, fetch tags,
check out the desired tag, and run the same locked install command. Do not use
managed `buzzx update` for a tagged checkout: the managed source path follows
only clean `main`.

### Main source install (managed updates)

The managed path follows `origin/main` from a clean `main` checkout. Install
from that checkout with:

```sh
git clone https://github.com/suraciii/buzzx.git
cd buzzx
cargo install --locked --path . --force
```

The command must be run from the checkout or one of its subdirectories. The
update implementation finds the repository by walking upward to a directory
containing `.git`, `Cargo.toml`, and package name `buzzx`.

The installed Cargo root must be the root that owns the running executable:
`current_exe` is `<cargo-root>/bin/buzzx` (or `buzzx.exe`), and its
`<cargo-root>/.crates2.json` entry must record this checkout as the `buzzx`
source install. A checkout or `target/` executable is rejected. This prevents
an unrelated or in-tree development binary from changing the repository.

## Update commands

The command uses flags; there is no subcommand form:

```text literal
buzzx update              # apply a managed source update
buzzx update --check      # inspect; no HEAD move and no Cargo build
buzzx update --plan       # show the apply plan; no HEAD move and no Cargo build
```

`--check` and `--plan` are mutually exclusive. `--prerelease` is meaningful
only for prebuilt release checks; a source update rejects it. Startup checks,
background checks, downloads, automatic restarts, and service restarts are not
performed.

### Prebuilt binaries

For a prebuilt binary, `buzzx update --check` and `buzzx update --plan` perform
the stable Release check (or include prereleases when `--prerelease` is given)
and provide the external installer path. They do not download or replace the
binary. Bare `buzzx update` returns `unsupported_install`: the release owner is
the external installer, not the source checkout.

### Source binaries

For an installed source binary, the source path takes precedence over a
generic release-check result. The command resolves the checkout first and
accepts only a clean `main` branch on Linux or macOS. It uses `origin/main` as
the only target.

`--check` and `--plan` may perform one bounded
`git fetch --no-tags origin main`. Fetch can modify only Git's remote-tracking
reference; it does not change HEAD, files, or the installed binary. The output
identifies the current and target commits. `--plan` also shows the exact
fast-forward and install commands and that a restart is required after apply.

A bare `buzzx update` records the current commit, verifies that it is an
ancestor of `origin/main`, and runs `git merge --ff-only origin/main`. If the
commits are equal it returns `up_to_date` without running Cargo. Otherwise it
runs:

```text literal
cargo install --locked --path . --force
```

Cargo output goes to stderr; stdout remains one JSON object. The newly
installed executable is run with `build-info` and must report the target
commit, `install_kind: source`, and a clean source identity. Only after that
verification does the command report success. The current process is still
the old binary, so a successful update always requires the user to restart
`buzzx`.

The command never auto-stashes, merges, resets a dirty tree, or restarts a
service. Dirty trees, detached HEAD, branches other than `main`, missing
`origin/main`, and divergent history stop without moving HEAD. On Windows,
a source install returns `blocked` with the manual command because automatic
replacement is not supported there.

On Cargo or post-install verification failure, the checkout is restored with
`git reset --keep` to the recorded previous commit. This rollback changes the
checkout only; it does not roll back a binary or configuration. The result is
`failed`, never success, and includes a manual reinstall command when the
installed binary may no longer match the checkout.

## JSON result and exit codes

Every update path writes one JSON object to stdout. Cargo logs may go to
stderr. Git diagnostics are suppressed from both JSON and stderr so remote
credentials cannot leak. The stable status and exit-code contract is:

| Status | Meaning | Exit code |
|---|---|---:|
| `up_to_date` | HEAD already equals `origin/main`; no install | 0 |
| `update_available` | `--check` or `--plan` found a newer target | 0 |
| `updated` | Fast-forward and new-binary verification succeeded | 0 |
| `blocked` | Dirty, wrong branch, detached, diverged, missing checkout, or Windows | 1 |
| `unsupported_install` | Prebuilt install; external installer owns updates | 1 |
| `unknown` | Fetch or target resolution failed | 2 |
| `failed` | Cargo install or new-binary verification failed | 4 |

The report contains the running/current and target commits as distinct fields;
unknown values are `null`. Source reports include `install_kind: source` and
the resolved `source_path`; prebuilt reports use the build identity when it
provides a commit. Errors use stable reasons and a safe message. Raw Git
diagnostics, credentials, auth tags, relay credentials, and config values
never appear in JSON or stderr.

For example, a successful source apply has this shape:

```json
{
  "status": "updated",
  "install_kind": "source",
  "source_path": "/work/buzzx",
  "branch": "main",
  "current_commit": "abcdef123456",
  "target_commit": "fedcba654321",
  "previous_commit": "abcdef123456",
  "restart_required": true,
  "error": null,
  "message": "updated from abcdef123456 to fedcba654321"
}
```

`current_commit` is the checkout commit at action start, `target_commit` is
the resolved `origin/main`, and `previous_commit` is the commit used for
rollback. They are not interchangeable.

Source reports also include `behind`, `commands`, `remote_refs_refreshed`,
and `rollback_succeeded`. These distinguish preview commands, fetched refs,
and an attempted rollback from a verified rollback. Fetch is bounded to 30
seconds; installed build-identity verification is bounded to 10 seconds.
Timeout cleanup terminates the external command's process group on Unix.

## First-release checklist

Before creating `v0.1.0`:

1. Finish the full repository checks and release acceptance.
2. Install one artifact on each available platform and record `--version`,
   `whoami`, and the live TUI smoke result.
3. Set `Cargo.toml` to `0.1.0`, merge it, and verify the tag matches exactly.
4. Push `v0.1.0`; inspect release assets and every checksum.
5. Verify a source checkout of `v0.1.0` reports the release version and a clean
   `main` checkout reports `0.0.0-main.g<sha>`.

The release is complete only when both installation paths state exactly how a
user installs and moves to the next version.
