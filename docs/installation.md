# Installation

`buzzx` ships as a prebuilt binary for Linux, macOS, and Windows. A user does
not need a Rust toolchain, and does not need WSL on Windows.

Every artifact below is built by the release workflow when a version tag is
pushed. They are attached to the release for that tag on
[GitHub](https://github.com/suraciii/buzzx/releases).

## Which artifact

| Platform | Architecture | Artifact |
|---|---|---|
| Linux | x86_64 | `buzzx-x86_64-unknown-linux-gnu.tar.xz` |
| macOS | Apple silicon | `buzzx-aarch64-apple-darwin.tar.xz` |
| macOS | Intel | `buzzx-x86_64-apple-darwin.tar.xz` |
| Windows | x86_64 | `buzzx-x86_64-pc-windows-msvc.zip` |

Each archive holds the binary and the README, and each one has a `.sha256`
file beside it. `sha256.sum` covers every artifact of the release:

```text literal
sha256sum -c buzzx-x86_64-unknown-linux-gnu.tar.xz.sha256          # Linux
shasum -a 256 -c buzzx-aarch64-apple-darwin.tar.xz.sha256          # macOS
```

```text literal
certutil -hashfile buzzx-x86_64-pc-windows-msvc.zip SHA256         # Windows
```

## Install script

The release also carries `buzzx-installer.sh` and `buzzx-installer.ps1`.

```text literal
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/suraciii/buzzx/releases/latest/download/buzzx-installer.sh | sh
```

```text literal
powershell -ExecutionPolicy Bypass -c "irm https://github.com/suraciii/buzzx/releases/latest/download/buzzx-installer.ps1 | iex"
```

Both scripts detect the machine they run on, download the matching archive,
verify its checksum, and install the binary to `$CARGO_HOME/bin`
(`~/.cargo/bin`, or `%USERPROFILE%\.cargo\bin`). They also put that directory
on `PATH`: the shell installer appends a line to the shell profiles, and the
PowerShell installer writes the user `Path` value in the registry. Both skip
that step when `BUZZX_NO_MODIFY_PATH=1` is set.

Set `BUZZX_INSTALL_DIR` to install somewhere else:

```text literal
BUZZX_INSTALL_DIR=$HOME/.local curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/suraciii/buzzx/releases/latest/download/buzzx-installer.sh | sh
```

```text literal
$env:BUZZX_INSTALL_DIR = "$env:USERPROFILE\.local"
irm https://github.com/suraciii/buzzx/releases/latest/download/buzzx-installer.ps1 | iex
```

A forced directory gets the binary in `<dir>/bin`. On Unix the installer also
writes an `env` helper script to `<dir>/env` - the parent of `bin`, the way
rustup lays it out - and sources that script from the shell profiles. On
Windows there is no helper script: the registry `Path` value above is the only
change.

## Manual install

Unpack the archive and put the binary on PATH:

```text literal
tar xf buzzx-x86_64-unknown-linux-gnu.tar.xz
install -m 755 buzzx-x86_64-unknown-linux-gnu/buzzx ~/.local/bin/buzzx
```

On Windows, unpack the zip and move `buzzx.exe` to a directory on PATH, or
run it from where it sits.

## The binaries are not signed

The release is not code-signed, so both platforms warn before the first run.
Neither warning means the download is damaged: the checksum files are the
integrity check.

- macOS: Gatekeeper quarantines a downloaded binary. Clear the attribute once
  after unpacking: `xattr -d com.apple.quarantine <path>/buzzx`.
- Windows: SmartScreen shows "Windows protected your PC" for a downloaded
  `buzzx.exe` or installer script. Choose "More info" and then "Run anyway".

Removing the warnings needs a paid certificate - an Apple Developer account
for macOS, and an EV certificate for Windows. That is a separate decision, and
nothing in the product depends on it.

## Build from source

A Rust toolchain at the version CI pins (1.97.1) builds `buzzx` on all three
platforms:

```text literal
cargo install --locked --git https://github.com/suraciii/buzzx
```

The lockfile is part of the contract, so `--locked` is required rather than
optional: a build that resolves newer dependencies is not the released build.

## First run

```text literal
buzzx login
```

`buzzx login` verifies the identity against the relay and writes the config
file; see [configuration.md](configuration.md#login). On Windows, run the TUI
in Windows Terminal rather than the legacy console host, which cannot render
the alternate screen.

## What each platform promises

The three platforms run the same code and the same gates. What differs is the
filesystem protection behind the config file: Unix mode bits on Linux and
macOS, and the user profile ACL on Windows. Both are documented under
[file permissions](configuration.md#file-permissions).
