# Decision: 0007 - Windows keeps the profile ACL, and says when it cannot

## Status

accepted

## Context

The config file holds a private key. On Unix the contract is mode bits:
`buzzx` creates the directory 0700 and the file 0600, and refuses to read a
file that another user can read, before touching its bytes. That contract is
two unconditional `std::os::unix` imports in `src/config.rs` and
`src/login.rs`, so the crate did not compile for a Windows target at all.

Making `buzzx` installable on Windows therefore needs an answer for what
protects the key there. Three candidates were considered: the ACL a file
inherits from the user profile, an owner-only DACL written through the
Windows API, and DPAPI encryption of the private key field.

## Decision

On Windows the protection is the ACL the file inherits from the user profile.
`buzzx` sets nothing, refuses nothing, and writes one warning to stderr when a
config or key file path is outside the profile.

Concretely:

- `src/platform.rs` owns the difference. `config.rs` and `login.rs` call
  `check_secret`, `restrict_file`, `restrict_dir`, `secret_create`, and
  `mode_note`, and keep the shape they had: mode-bit enforcement on Unix, a
  warning on Windows, and no exit code in the platform module.
- The ACL is inherited rather than created. A file under `%APPDATA%\buzzx\`
  cannot be read by other standard users, and can be read by an
  administrator, which is the line Unix modes draw too: any user who can run
  `sudo` reads a 0600 file.
- The comparison is lexical: both paths are made absolute with
  `std::path::absolute`, then compared component by component without case.
  It answers "does this placement keep the key where the profile protects
  it", not "does this path physically resolve inside the profile". A junction
  that points out of the profile is the user's own arrangement.
- Unix behavior does not change, including the refusal message that names
  `chmod 600`.

## Consequences

- Windows users can log in, keep a config file, and read it back; the file
  stays a plain TOML file they can edit, copy, and move between machines.
- A Windows user who points `BUZZX_CONFIG` or `--private-key-file` outside
  the profile gets a warning on each run. The warning is the whole
  enforcement, so it must not read as a refusal: the run continues.
- The Windows branch is verified by the `Portability (windows-latest)` CI job,
  which builds and runs the test suite on a real Windows runner. No maintainer
  machine runs Windows, so a Windows-only regression is caught in CI or not at
  all.
- If the warning turns out to be too weak, the same `platform.rs` boundary
  can carry an explicit DACL without touching a call site.

## Alternatives considered

**Write an owner-only DACL on every config write.** This is strictly stronger:
it protects the file even if it is moved outside the profile, and it does not
depend on inheritance. Rejected for the first Windows release because it needs
the `windows` or `windows-sys` crate, unsafe FFI, and a failure path for a
caller who is not the file's owner - and an administrator can take ownership
anyway, so the strength it adds over the profile ACL is narrow. It remains the
first hardening step, and the reason `platform.rs` exists as a boundary.

**Encrypt the private key field with DPAPI.** This is the strongest answer to
a copied file: the ciphertext is bound to the user account and cannot be read
on another machine. Rejected because it ends the file's life as the user's own
text - the field can no longer be read, hand-edited, or moved - and the same
mechanism would have to be decided for Linux and macOS, where nothing is
wrong. It becomes a product decision about key storage, not a portability fix.

**Refuse to read a config outside the profile on Windows.** This matches the
Unix refusal in shape and is the tempting symmetry. Rejected because it
converts a path the user chose into an unusable login with no remedy the user
can apply from the tool. On Unix a refusal names a command the user can run;
on Windows the honest statement is "cannot promise", not "proved shared".
