# Versioning and updates

This document is the product contract for the version shown by `buzzx` and for
the two supported installation paths: a prebuilt release and a build from
source. It is intentionally separate from the relay protocol. The relay is a
service that `buzzx` connects to; it is not the binary distribution source.

## The release is not ready yet

The repository currently has no published tag. `Cargo.toml` contains the
planned package version (`0.1.0`), but a binary built from `main` is not a
`0.1.0` release. Do not publish `v0.1.0` until the release acceptance in
[`eng/release.md`](../eng/release.md) and the platform checks in the release
runbook pass.

When the product is ready for a first public candidate, use a prerelease tag
such as `v0.1.0-rc.1`. Publish `v0.1.0` only after the candidate has passed
the same install and live-relay checks that a user will depend on.

## Version vocabulary

There is one release version. `Cargo.toml`, the Git tag, the GitHub Release,
the archive names, and the version reported by an exact release build must
agree on the SemVer value.

| Build | Displayed version | Channel | Update meaning |
|---|---|---|---|
| Exact stable tag `v0.1.0` | `0.1.0` | `stable` | Compare with the latest stable release |
| Exact prerelease tag `v0.1.0-rc.1` | `0.1.0-rc.1` | `prerelease` | Compare only when prereleases are requested |
| Clean `main` commit | `0.0.0-main.gabcdef123456` | `dev` | Never claim it is a stable release |
| Clean non-main commit | `0.0.0-dev.gabcdef123456` | `dev` | Never claim it is a stable release |
| Dirty source tree | The same value with `.dirty` appended | `dev` | Warn that the binary is not reproducible |
| Source without Git metadata | `0.0.0-dev.unknown` | `dev` | Show the source-install instructions |

`gabcdef123456` is a fixed-length, 12-character short Git object id with a
`g` prefix. The `main` and `dev` values are valid SemVer prerelease
versions. They are runtime build identities, not versions that are edited into
`Cargo.toml` on every commit.

`v0.0.0-main.gabcdef123456` is therefore a valid **snapshot tag** if a
maintainer deliberately publishes one, but it is not the normal workflow. Do
not create a GitHub Release for every main commit: the current cargo-dist
workflow treats numeric SemVer tags as release input, and a tag per commit
would create noisy, misleading releases. If a binary snapshot is needed, use
a CI artifact or a deliberately named prerelease, and mark it as `dev` so the
stable updater ignores it.

The build identity is injected by the build/release process. The source of
truth for a stable version remains the package version and its matching tag;
the Git hash only identifies an untagged development build. A version string
must not contain a timestamp because that would make the same commit produce a
different identity.

## What the user sees

`buzzx --version` stays a one-line, script-friendly answer:

```text literal
buzzx 0.1.0
```

For an untagged main build it is, for example:

```text literal
buzzx 0.0.0-main.gabcdef123456
```

The TUI About surface carries the information needed for support without
making the channel header noisy:

```text literal
buzzx 0.0.0-main.gabcdef123456
channel: dev
source: main @ abcdef123456 (clean)
install: source
target: x86_64-unknown-linux-gnu
relay: buzz.surac.cloud
config: ~/.config/buzzx/config.toml
```

The `?` help surface shows the version and channel, and `Ctrl+P → About`
opens the complete diagnostic view. The status bar remains reserved for
connection, read, and write state. A source build and a prebuilt binary are
both valid clients; `install: source` is provenance, not a compatibility
failure.

## Installation paths

Users choose one path and keep using its update procedure. The client does not
silently change a source installation into a prebuilt installation.

### Prebuilt release (recommended for regular users)

The release workflow is the existing cargo-dist path:

```text literal
merge main → set the package version → run the full checks
→ push vX.Y.Z → build Linux/macOS/Windows artifacts
→ publish the GitHub Release, notes, and SHA-256 files
```

Linux and macOS use the release `buzzx-installer.sh`; Windows uses
`buzzx-installer.ps1`. Manual archive installation remains available. After
installing, verify:

```text literal
buzzx --version
buzzx login
buzzx whoami
```

The installer changes the binary and PATH only. It does not remove the config,
private key, relay preference, or read state. The unsigned-release warnings
described in [`installation.md`](installation.md) remain visible to the
user.

### Stable source build (reproducible)

Use a release tag when the source build is meant to reproduce a published
version:

```sh
git clone https://github.com/suraciii/buzzx.git
cd buzzx
git checkout v0.1.0
cargo install --locked --path . --force
```

The installed command reports `buzzx 0.1.0`, while About identifies the
provenance as `source`. The pinned Rust toolchain and `Cargo.lock` are part
of the reproducibility contract; `--locked` is required.

To update a stable source installation, select the new tag and reinstall:

```sh
git fetch --tags origin
git checkout v0.1.1
cargo install --locked --path . --force
```

Do not use an unreviewed `git pull` when the goal is to reproduce a release.

### Main/source development build

For the current unreleased product, build from a pinned commit or from
`main`:

```sh
git clone https://github.com/suraciii/buzzx.git
cd buzzx
git checkout <commit>
cargo install --locked --path . --force
```

The binary reports `0.0.0-main.g<short-sha>` for a clean `main` commit. To
update it, choose the next commit deliberately and reinstall:

```sh
git fetch origin
git checkout <new-commit>
cargo install --locked --path . --force
```

If the checkout is dirty, stop before replacing the binary and either commit,
stash, or discard the local changes explicitly. The client must not overwrite
work the user has not chosen to lose.

## Checking for updates

`buzzx update check` is an explicit, read-only check against the canonical
GitHub Releases metadata. It is not required for login, channel reads, or
sending. Startup checks are off by default.

For a prebuilt stable binary, the check can return `up_to_date` or
`update_available`. For a source build it returns `source_build`, including
the current commit, the latest stable release if one exists, and the command
to update the checkout. It must never report a development build as “the
latest stable version”. Offline, rate-limited, malformed, and unavailable
metadata return `unknown` with a retryable reason.

The machine-readable result contains at least:

```json
{
  "current_version": "0.0.0-main.gabcdef123456",
  "latest_stable_version": "0.1.0",
  "status": "source_build",
  "channel": "dev",
  "install_kind": "source",
  "source_commit": "abcdef123456",
  "release_url": "https://github.com/suraciii/buzzx/releases/tag/v0.1.0",
  "update_command": "git checkout v0.1.0 && cargo install --locked --path . --force"
}
```

The TUI may show a non-blocking `update available` notice and an About/update
panel. It does not execute a shell command, download a binary, replace the
running executable, or restart the session. The current process remains on
the version shown in About until the user installs and restarts it.

## First-release checklist

Before creating `v0.1.0`:

1. Decide the release scope and finish the full repository checks.
2. Run the installed-binary acceptance on Linux and the real-machine checks
   available for macOS and Windows.
3. Set `Cargo.toml` to `0.1.0`, merge that change, and verify the tag will
   match it exactly.
4. Push `v0.1.0`; inspect the GitHub Release assets and every checksum.
5. Install one artifact on each available platform and record `--version`,
   `whoami`, and the live TUI smoke result.
6. Verify a source checkout of `v0.1.0` reports the same release version and
   that a clean `main` checkout reports a `0.0.0-main.g<sha>` development
   version.

The public release is complete only when both installation paths are honest:
the prebuilt path is easy to install, and the source path tells the user
exactly which commit they are running and how to move to the next one.
