# Decision: 0003 - The owner's observation plane is out of scope

## Status

accepted

## Context

Two read-only investigations against `block/buzz` were run to establish
buzzx's macro product shape. They settled three facts, each cited.

1. The agent job protocol (kinds 43001–43006) is fully specified, stored,
   channel-scoped, member-readable, and already subscribed to by the mobile
   client. **Nothing emits it.** The harness publishes ordinary stream
   messages instead.
2. The live observation plane is kind 24200: ephemeral (20000–29999),
   NIP-44-encrypted to the owner, p-gated, and never stored. Rate limited and
   `since=<now>` only; no historical replay by contract.
3. Mid-turn steering needs no special transport. The default inbound policy
   cancels the in-flight turn and re-dispatches a merged prompt framing the
   new events as a *steering message*, carried by an ordinary stored channel
   message.

An earlier draft of the product contract made the observation plane buzzx's
reason to exist: decrypt 24200 frames, render a turn transcript, and let a
user cancel a turn. The third fact broke that framing. Steering is already
a channel message, so it needs no work; and the plane it would have motivated
is owner-private by protocol, rendered by desktop, and absent from the channel
timeline a member client reads.

## Decision

buzzx does not subscribe to kind 24200, does not decrypt observer frames, and
exposes no agent telemetry or control surface. It is a chat client. It renders
what a channel member reads over the relay's normal surfaces.

Concretely:

- No 24200 subscription, no `--follow-agent` flag, no telemetry view, no
  cancel-turn control frame.
- `buzzx watch` streams channel-scoped events to stdout as JSON. That is the
  session-requiring operation a one-shot client cannot perform, and it needs
  no protocol work beyond what the TUI already does.
- Steering an agent is a plain `buzzx send`, which is upstream's default path.

## Consequences

- The differentiator against `buzz-cli` is the held subscription, not the
  agent plane. A held subscription is also the thing that cannot be built on
  top of an ephemeral kind that the relay refuses to store, which is why 24200
  was examined and dropped rather than adopted.
- Activity a user wants to see must be channel content. If a workflow
  wants terminal-visible agent reporting, it emits stream messages, and buzzx
  shows it with no new code. This is the productive direction the decision
  creates.
- If a real terminal need for telemetry appears, it is a new decision record,
  not a leak into this one. The state is not explored here on purpose.

## Alternatives considered

**Render the 24200 stream in the TUI.** Gives the owner a live turn transcript
in the terminal, which desktop also has. Rejected because it costs NIP-44
decryption, an ownership model, and a bounded ring buffer for a surface whose
audience is the agent's owner — who is, in this product's own scenario, the
person who can already reach desktop. It also promotes an owner-private plane
into a member-visible client, which the protocol deliberately separates.

**Design against the job kinds 43001–43006.** Clean and member-readable, so a
terminal client could show job progress as first-class rows. Rejected because
no producer exists upstream. Designing a client surface against an
unimplemented type is designing against a hypothesis.

**Ship neither plane but keep the wording open in `core.md`.** Rejected: the
contract must be testable, and an unstated-but-hinted scope is the state that
produced the contradiction in the first place.
