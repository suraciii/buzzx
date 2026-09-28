# Continuous Integration

CI is the repository's verification contract, run remotely. The workflow
[ci.yml](../.github/workflows/ci.yml) runs the gates that `just check` runs
locally, on every pull request and on every push to `main`. A change that
fails a gate fails in the repository too.

CI runs the contract, not the product. It starts no relay, signs no event, and
installs no identity.

## Jobs

Three jobs run, and each one owns a toolchain:

| Job | Runs | Proves |
|---|---|---|
| Docs | `just docs-check size-check` | The documentation rules hold, the gate tests pass, and no Markdown document exceeds the 1000 line ceiling. |
| Rust | `just rust-check` | The code is formatted, lints clean under `-D warnings`, and passes its unit tests, on Linux. |
| Portability (`macos-latest`, `windows-latest`) | `just rust-check` | The same Rust gates on the two other targets a user can install `buzzx` on. |

Together they run every recipe of `just check`, and the Rust recipes on all
three targets. The commands live in the [Justfile](../Justfile); the workflow
installs the tools and names the recipes, so a command that changes in the
Justfile changes in CI with it. A new gate adds its recipe to the Justfile and
names it in the job that owns its toolchain.

The Portability job runs the Rust gates and not the documentation gates. The
gate scripts read files and compare strings, so the operating system cannot
change their verdict, and the Windows image makes no promise about the
`python3` alias. A platform assumption in the Rust code is what the job is
for, and the two extra targets are where one shows up.

The Rust recipes pass `--locked`, so CI also proves the committed `Cargo.lock`
is current: a `Cargo.toml` change that did not update the lockfile fails.

A new push to a pull request cancels the run in flight for that pull request;
every push to `main` starts its own run.

## Environment

- The Docs and Rust jobs run on `ubuntu-latest`; the Portability job runs on
  `macos-latest` and `windows-latest`. The repository is public, so runner
  minutes cost nothing on any of the three images.
- Rust is pinned to 1.97.1 with `clippy` and `rustfmt`. The local toolchain
  should be the same version, because a lint that one release reports and
  another does not would pass locally and fail in CI.
- Python is the ubuntu runner's `python3`. The gate scripts and their tests
  import the standard library only.
- `just` is installed with `taiki-e/install-action` on every runner, and every
  action is pinned to a full commit SHA with its version in a comment. The
  Justfile recipes run under `sh`; the Windows image provides one through the
  Git installation it puts on PATH.
- The workflow holds `contents: read` and reads no secret. A pull request from
  a fork cannot reach a credential, because none exists here. The release
  workflow is the only one with `contents: write`, and it is described in
  [release.md](release.md).

## Cache

Every Rust job caches `~/.cargo/registry`, `~/.cargo/git`, and `target`, keyed
by the `Cargo.lock` hash and the runner's operating system. Cargo fingerprints
decide what to rebuild, so a stale cache costs time and never correctness. The
Docs job caches nothing and finishes in seconds.

The run page shows what each step took. If the cache stops paying for itself,
remove `target` first: the registry and git paths still avoid the downloads.

## What CI does not run

- The live-relay acceptance. A change to the session, the transport, or the
  render contract still needs one live run against a real relay; CI has no
  relay, no identity, and no way to see how the relay canonicalizes an event.
  That run stays a local step, and a green workflow does not replace it.
- Packaging, publishing, and deployment. `ci.yml` builds nothing that leaves
  the runner. That work lives in a separate tag-triggered workflow; see
  [release.md](release.md).

## Enforcement

GitHub reports the jobs as `CI / Docs`, `CI / Rust`, `CI / Portability
(macos-latest)`, and `CI / Portability (windows-latest)`. The repository
ruleset `main` covers the default branch, so the checks decide whether a
change lands. The ruleset requires a pull request and the two ubuntu checks
from the GitHub Actions app, refuses a branch delete and a force push, and
names no bypass actor. A direct push to `main` fails, and the merge button
stays disabled until the required checks report success on the pull request
head.

The two Portability checks report on every change and are not required yet:
making them required is a ruleset change the owner makes in the repository
settings, and until then a red Portability job informs a review instead of
blocking a merge.

Two settings stay deliberately open, and both match a repository with one
maintainer. The ruleset does not require the head to be up to date with
`main`, so a merge needs no rebase once the checks pass. It also requires no
approving review, because an author cannot approve their own pull request:
the pull request is the gate, and the checks are the evidence.
