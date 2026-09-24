# buzzx developer entry points.
#
# The verification targets are the contract a commit must satisfy. Run
# `just check` before every push.

# Run every local gate
check: rust-check docs-check size-check

# Format check, lint, and unit tests. The cargo commands pass --locked: the
# committed lockfile is part of the contract, so a stale one fails here.
rust-check:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo test --locked

# Run the documentation gate and its own tests
docs-check:
    python3 -m unittest discover -s tests -p '*_test.py'
    python3 scripts/check-docs.py

# Run the documentation gate only, without its own tests
size-check:
    python3 scripts/check-file-sizes.py
