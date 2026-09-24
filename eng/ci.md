# Continuous Integration

CI is the repository's verification contract, run remotely. The workflow
[ci.yml](../.github/workflows/ci.yml) runs the gates that `just check` runs
locally, on every pull request and on every push to `main`. A change that
fails a gate fails in the repository too.

CI runs the contract, not the product. It starts no relay, signs no event, and
installs no identity.

## Jobs

Two jobs run in parallel, and each one owns a toolchain:

| Job | Runs | Proves |
|---|---|---|
| Docs | `just docs-check size-check` | The documentation rules hold, the gate tests pass, and no Markdown document exceeds the 1000 line ceiling. |
| Rust | `just rust-check` | The code is formatted, lints clean under `-D warnings`, and passes its unit tests. |

Together the two jobs run every recipe of `just check`. The commands live in
the [Justfile](../Justfile); the workflow installs the tools and names the
recipes, so a command that changes in the Justfile changes in CI with it. A
new gate adds its recipe to the Justfile and names it in the job that owns its
toolchain.

The Rust recipes pass `--locked`, so CI also proves the committed `Cargo.lock`
is current: a `Cargo.toml` change that did not update the lockfile fails.

A new push to a pull request cancels the run in flight for that pull request;
every push to `main` starts its own run.

## Environment

- The runner is `ubuntu-latest`. The repository is public, so the runner
  minutes cost nothing, and no gate needs a different image.
- Rust is pinned to 1.97.1 with `clippy` and `rustfmt`. The local toolchain
  should be the same version, because a lint that one release reports and
  another does not would pass locally and fail in CI.
- Python is the runner's `python3`. The gate scripts and their tests import
  the standard library only.
- `just` is installed with `taiki-e/install-action`, and every action is
  pinned to a full commit SHA with its version in a comment.
- The workflow holds `contents: read` and reads no secret. A pull request from
  a fork cannot reach a credential, because none exists here.

## Cache

The Rust job caches `~/.cargo/registry`, `~/.cargo/git`, and `target`, keyed
by the `Cargo.lock` hash. Cargo fingerprints decide what to rebuild, so a
stale cache costs time and never correctness. The Docs job caches nothing and
finishes in seconds.

The run page shows what each step took. If the cache stops paying for itself,
remove `target` first: the registry and git paths still avoid the downloads.

## What CI does not run

- The live-relay acceptance. A change to the session, the transport, or the
  render contract still needs one live run against a real relay; CI has no
  relay, no identity, and no way to see how the relay canonicalizes an event.
  That run stays a local step, and a green workflow does not replace it.
- Packaging, publishing, and deployment. The workflow builds nothing that
  leaves the runner.

## Enforcement

GitHub reports the jobs as `CI / Docs` and `CI / Rust`. `main` is not
protected, so a red run on `main` is a signal after the fact, and a run on a
pull request is a signal before the merge. Whether the checks must pass is a
repository setting, not a workflow fact.
