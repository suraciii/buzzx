# Testing

The `buzzx` test rules. A test explains the rules and the edge cases of a real
contract. A test that proves a call, a copy, or a text literal is a fragility
that survives as coverage theater.

## The gates are tested first

`scripts/check-docs.py` and `scripts/check-file-sizes.py` are the only code in
this repository right now, so they are the only code under test. Their tests
live in `tests/check_docs_test.py` and `tests/check_sizes_test.py`, and they run
against synthetic trees rather than the real documents, so a broken matcher
fails instead of staying silent.

Run both:

```bash
python3 -m unittest discover -s tests -p '*_test.py'
```

## What needs a test

Every contract with observable behavior. For the documentation gates that means
each rule must have a case that fires it and, where the rule has a permitted
side, a case that passes it:

- `latin-script-prose-only` rejects a CJK ideograph and a Greek letter, and
  accepts an accented Latin letter such as a cafe with a resume file.
- `ascii-diagram-only` rejects a mermaid fence and rejects a Unicode arrow
  inside a `text diagram` fence.
- `text-fence-kind-required` rejects a bare text fence and accepts one that
  declares `literal` or `diagram`.
- `markdown-link-reference-exists` rejects an undefined reference and accepts
  a defined one.
- `relative-link-target-exists` rejects a missing target and rejects a link
  that leaves the repository.
- `markdown-heading-fragment-exists` rejects a missing fragment and accepts
  one that names a real heading.
- `required-document-exists` and `documentation-root-exists` report the
  missing file or directory.
- `decision-record-status` and `decision-record-alternatives` reject a record
  that omits the section, and accept a record that carries both.
- A fenced block is exempt from the prose rules.

For the file size gate: a document at the ceiling passes, one over the ceiling
fails, a final line without a trailing newline is counted, and an empty file is
zero lines.

## What does not need a test

- Wiring: a call that forwards to a function already tested.
- Copies: a `From` or `Into` impl whose fields are already covered.
- Source text, a definition, or a link that resolves.
- Mock echoes, which assert that a mock stores what you gave it.
- A bare `unwrap()` that only checks it does not panic.
- An empty result or a length assertion with no boundary attached to it.
- A test whose expected value is what the implementation already produces,
  such as a duplicate of a real row on the same test path.

For these, write a throwaway script, run it once, and delete it.

## Fakes, not mocks

A fake holds state, asserts nothing, and returns what the test asks for in the
order the test asks. A test reads like a session. A mock that asserts a call is
a test of the call, not the behavior.

## Determinism

A test with the same inputs always produces the same result. Do not rely on
wall-clock time, network, or a filesystem cache. When a hash or a set order
matters, sort by a stable key before the assertion.
