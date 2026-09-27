# Web surface

Status: proposed. [The product specification](../docs/web.md) owns startup,
scope, browser interaction and acceptance. No Web implementation exists at
the recorded baseline.

## Design pressure

The largest risk is copying the TUI's rules into a second application. A web
page can look correct while choosing a different reply root, clearing unread
too early or retrying an uncertain write. The Web surface must reuse those
decisions, while giving the browser responsibility for layout and input.

The current [architecture](architecture.md) separates one-shot relay
operations from held-session behavior. The [shared core](shared-core.md)
already owns identity-aware reads and signed writes, but subscriptions,
read progress, Agent summaries and conversation state also live above it.
Reusing only `client.rs` is therefore insufficient for parity.

## Alternatives considered

| Approach | Boundary | Main failure mode |
| --- | --- | --- |
| Serve a native browser view from the buzzx process | Existing Rust client/session owns relay behavior; browser owns presentation | Requires an explicit browser view lifecycle and local-access boundary |
| Browser connects to the relay directly | A second client owns credentials, events and session rules | Duplicated signing, mention, read-state and retry logic can drift |
| Stream the terminal UI into a browser | Existing TUI owns both semantics and presentation | Terminal interaction remains; native selection, accessibility and responsive browser behavior are restricted |

Choose the first approach. Keep one distributed buzzx executable with bundled
assets. Do not introduce a hosted service, browser identity store, new relay
API, generic plugin framework or new shared crate for this slice.

## Ownership

```text diagram
 browser view: layout, input, focus, scroll, visibility
      |
 authenticated local requests and live updates
      |
 buzzx process: browser lifecycle and session coordination
      |
 shared conversation behavior + existing session pumps
      |
 existing client, identity, read-state and Agent logic
      |
 existing HTTP bridge and relay WebSocket
```

The browser submits user actions with explicit conversation and target ids.
The process validates them against that view's state and accessible data.
The browser cannot submit arbitrary signed events, signing requests, relay
URLs or identity overrides. Reply routing, mention membership, ownership,
read-marker merging and write classification remain authoritative in Rust.

Extract only the session behavior needed to serve both interactive surfaces.
Terminal key mapping, ratatui layout and terminal dimensions stay TUI-specific.
Web view state must not reuse terminal dimensions as a proxy for visibility.
Keep the dependency direction in [architecture.md](architecture.md); no
second implementation of the relay bridge or event builders is permitted.

The local API is private to the bundled page. Its endpoints and frontend
framework are implementation choices, not a new public automation contract.
Implementation must define and test the action/update schema before exposing
it, including view identity, operation correlation and explicit write outcomes.

## State and recovery

The process owns the configured identity and relay for its lifetime. Each
browser view owns its independent selection, draft, target and presentation
state. Share relay knowledge where appropriate without letting one view's
navigation overwrite another's. No durable draft or message database is added.

Presentation reports identify the actual conversation, latest visible event
and view generation. Only current, visible and focused views may report read
progress. Reject late reports after navigation or disconnect. Feed valid
reports into the existing read-state rules; do not invent a Web marker model.

Browser-to-process recovery is distinct from process-to-relay recovery.
After either disconnect, recover reads and state before declaring the view
current. Late responses cannot replace a newer selection or overwrite newer
edits, deletions or live messages. Preserve current TUI bounded reads and
coverage signals rather than adding a new pagination model.

Correlate a submitted UI operation across a temporary browser connection
loss. If the process already accepted the operation, returning a view or
repeating the same operation reference must not sign it again. Read the
existing result, or show it as uncertain when it cannot be established.
This is local duplicate suppression, not permission to retry relay writes.
Do not queue writes offline or replay them after process restart.

The Web guard against publishing while disconnected is a browser interaction
requirement. It does not change the underlying `Stored`, `Refused` and
`Unknown` meanings or require changing TUI connection policy in this slice.

## Local access and content

The process can sign as the configured identity, so a loopback port alone is
not authentication. Only pages authorized through this run's terminal access
link can read private data or request actions. Use an unpredictable, run-scoped
bootstrap capability to establish an HttpOnly browser session; keep bootstrap
material out of URL query strings, request logs and referrers, remove it from
the visible URL after use, and invalidate sessions when the process ends.
The terminal link may authorize another tab during the same run.

Validate the loopback Host and the browser Origin on local endpoints and live
connections. Reject cross-origin requests and require session-bound protection
on state-changing requests. No wildcard CORS, unauthenticated loopback API or
non-loopback listen fallback is allowed. An SSH forward is a transport to the
same local session, not a supported public hosting configuration.

Keep the private key and NIP-OA auth material inside the process. Send only
the identity's public label and authorized view data to the browser. Do not
persist credentials or private conversation data in localStorage, IndexedDB,
service-worker caches or a new on-disk store. Mark private responses no-store.

Treat every relay-derived name, message, link and error as untrusted content.
Rendering must not execute raw HTML or scripts, create active unsafe URLs or
fetch remote media automatically. Bundle scripts, styles and fonts; restrict
their origins and prevent framing by other websites. Apply the same protection
to Agent details and errors as to message bodies.

Forward only the existing permitted Agent summary. Do not expose raw observer
frames, transcripts, tool arguments or inaccessible context metadata to the
page. An invalid browser session stops local API access and displays an access
error. Relay authorization or membership loss follows the product's blocked
action and retained-draft rules; it never changes identity automatically. A
transient local connection loss may retain the existing page's loaded content
with a stale label.

## Delivery sequence

1. Add the command, packaged assets, local authorization and read-only live
   conversation view. Prove startup, stop and key isolation end to end.
2. Add the complete message-action path with mentions and write outcomes.
   Prove stored routing and non-retry behavior before broadening the UI.
3. Complete Inbox/read state, focused threads, My agents, help and responsive
   interaction using the existing semantics. This completes the parity matrix.
4. Run comparative TUI/Web acceptance and browser boundary checks against the
   final packaged binary. A milestone is not the full Web release until all
   rows pass. Update the architecture map to the actual module ownership.

## Verification

Keep the repository checks and add behavior-level browser coverage during
implementation. Exercise the existing TUI suite after shared behavior moves.
Use deterministic fixtures for write refusal, lost answers, late responses,
two-tab isolation, hidden-tab read progress and Agent freshness.

Verify that a bare unauthenticated local request and an untrusted website
cannot read or send as the identity; reject invalid Host/Origin, forged
state-changing requests and credentials from an earlier process. Inspect
responses, browser storage and logs for key/auth-tag leakage. Render hostile
message/profile/error fixtures and prove they remain inert.

Use a real relay for the product's interaction loop, with stored-event and
recipient-authenticated readback. Keep screenshots, protocol evidence and
CI results separate. A static wireframe, mock response or source inspection
cannot establish functional parity.
