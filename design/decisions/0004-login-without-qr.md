# Decision: 0004 - Login takes a key, not a QR code

## Status

accepted

## Context

`buzzx login` is the onboarding path for a terminal user. The mobile app
onboards by scanning a QR code, and a scan entry was requested for parity.
An investigation of the mobile pairing surface found one protocol: NIP-AB
pairing (`buzz pair`, `nostrpair://` URIs). It moves an identity the other
way: an already-authenticated CLI or desktop transfers its relay URL and
key material *to the phone*. Terminal login needs the opposite direction -
the phone holds the identity and authorizes a session for the terminal -
which is remote signing in the shape of NIP-46. No such protocol is
confirmed on the mobile client.

## Decision

`buzzx login` ships with three key inputs - flag, 0600 file, stdin - plus
the environment and an interactive wizard. The wizard lists the scan entry
and answers it honestly: scan login is not available, because no mobile
remote-signing protocol is confirmed. No pairing transport is improvised.

Concretely:

- No QR rendering, no `nostrpair://` or NIP-46 client, no challenge
  protocol, and no claim that scan login works.
- The wizard's scan choice stays in the menu as the honest state of the
  product: listed, and refused with a reason.
- Scan pairing becomes possible only when a mobile remote-signing protocol
  is confirmed, and it lands as a new decision record with end-to-end
  evidence: real phone approval, refusal, expiry, replay resistance, and
  revocation.

## Consequences

- Login today never transports a key over the network in either direction.
  The only bytes that leave the machine are the ones a session already
  sends: a NIP-42 AUTH event on one WebSocket connection, which doubles as
  verification that the relay accepts the identity.
- A headless user onboards with `--private-key-file` or
  `--private-key-stdin`; the wizard exists for the terminal case. No path
  requires a phone.

## Alternatives considered

**Implement scan login over NIP-AB now.** Rejected because the protocol
moves the long-term key to the phone, not a session grant to the terminal.
Adopting it would either transfer the terminal's key out - the opposite of
what a login needs - or require a private variant of the protocol, which is
the improvised transport this decision forbids.

**Ship a private challenge-over-relay protocol.** A QR with relay, nonce,
expiry, and a temporary public key, and a phone app able to authorize it.
Rejected: it works only after both clients agree on it, and a second
implementation of an unagreed protocol is a fork of the product, not a
client of it.

**Hide the scan entry until it works.** Rejected: the menu is the product's
honest answer to "how do I log in from my phone", and an unexplained
absence reads as a gap rather than a decision.
