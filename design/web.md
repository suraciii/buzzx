# Web surface

Status: the loopback Web surface is implemented in the current working-tree
baseline (`src/web.rs`); it is not yet in a committed release. Community
profile selection and switching are specified here as contract for work in
progress: they are not implemented and are not parity evidence yet.
[The shared catalog](../docs/interactive.md) owns scope; [the Web
specification](../docs/web.md) owns startup and browser interaction.

## Design pressure

The largest risk is copying the TUI's rules into a second application. A web
page can look correct while choosing a different reply root, clearing unread
too early or retrying an uncertain write. The Web surface must reuse those
decisions, while giving the browser responsibility for layout and input.

The current [architecture](architecture.md) separates one-shot relay
operations from held-session behavior. The [shared core](shared-core.md)
already owns identity-aware reads and signed writes, but subscriptions,
read progress, Agent summaries, search/context state, history windows, reader
anchors and draft-safe return also live above it.
Reusing only `client.rs` is therefore insufficient for parity.

[The Desktop/Mobile comparison](client-reference.md) informs presentation and
navigation. Their newer relay contracts and richer feature sets do not replace
buzzx's current transport or product rules.

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
existing HTTP bridge and relay WebSocket of the active community
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
Reuse the current search/context/history operations and their limits in
`client.rs`, with shared request generations, anchors and draft guards above
them. Do not duplicate that knowledge in browser-only stores or call TUI keys
to simulate it.

The local API is private to the bundled page. Its endpoints and frontend
framework are implementation choices, not a new public automation contract.
Implementation must define and test the action/update schema before exposing
it, including view identity, operation correlation and explicit write outcomes.

## State and recovery

The process owns the configured identity for its lifetime and one active
community profile at a time. Switching re-binds the single relay connection
inside the process instead of adding a second one; see
[community switching](#community-switching). Each browser view owns its
independent selection, draft, target and presentation state. Share relay
knowledge where appropriate without letting one view's navigation overwrite
another's. No durable draft or message database is added.

Presentation reports identify the actual conversation, latest visible event
and view generation. Only current, visible and focused views may report read
progress. Reject late reports after navigation or disconnect. Feed valid
reports into the existing read-state rules; do not invent a Web marker model.

Browser-to-process recovery is distinct from process-to-relay recovery.
After either disconnect, recover reads and state before declaring the view
current. Late responses cannot replace a newer selection or overwrite newer
edits, deletions or live messages. Use the current incremental channel/thread
history implementation, bounded search and coverage signals. The browser
adapts input and rendering; it does not own another pagination algorithm.
Carry the actual Search/Context/Thread/Reader origin through updates so Back
cannot reconstruct its destination from whichever channel happens to be active.

Correlate a submitted UI operation across a temporary browser connection
loss. If the process already accepted the operation, returning a view or
repeating the same operation reference must not sign it again. Read the
existing result, or show it as uncertain when it cannot be established.
This is local duplicate suppression, not permission to retry relay writes.
Do not queue writes offline or replay them after process restart.

Loss of the local browser connection prevents submitting a new action. Relay
publication eligibility comes from the shared session rules, with the same
`Stored`, `Refused` and `Unknown` meanings in both interactive surfaces.

## Community switching

Switching the active community is a process-level operation. One
`buzzx web` process holds one relay connection, so every tab observes the
same active profile. The browser may list, select, rename and remove saved
profiles; it never submits a relay URL, an auth tag or identity material, so
adding a community stays a terminal action and the page only prompts for it.
A profile's auth tag reaches the browser only as configured or not
configured, never as content.

The switch follows one sequence:

1. Any tab may request a switch to a saved profile. A pending or uncertain
   write in any tab blocks the request, naming the tab and the operation.
2. The process closes the old subscriptions, connects the target relay and
   completes NIP-42 with the target profile's auth tag. It broadcasts
   `community_switching` to every view; views clear their visible targets
   and show the switching state instead of passing old content off as the
   target community.
3. On success the process publishes the new `community_id` with a new view
   generation to every tab; each view restores that profile's recent view or
   the Inbox.
4. On failure every tab shows the target profile's error and Retry. The
   process does not fall back to the previous profile or another one; the
   person chooses.

Every browser action and update carries `view_id`, `community_id` and
`generation`. Updates from an old generation - events, read reports and
operation results that arrive after a switch - are dropped, not merged into
the new community. The process re-checks permissions, membership and targets
against the active profile, so a forged `community_id` grants nothing and one
view's selection never retargets another view. Drafts are keyed by view,
community and conversation; a switch suspends them into the previous
profile's slots instead of carrying them across.

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
3. Complete the shared find-and-resume journey: conversation lookup, search
   filters, exact context, adjacent history pages, focused thread, complete
   reader and draft-safe return. Include Inbox/read state, My agents, help and
   responsive interaction. Every shared catalog row is required.
4. Run comparative TUI/Web acceptance and browser boundary checks against the
   final packaged binary. A milestone is not the full Web release until all
   rows pass. Update the architecture map to the actual module ownership.

5. Add the community selector on top of a session that can re-bind its one
   relay connection: process-wide switching, generation-tagged updates and
   terminal-only additions, proven with two relays and two tabs before the
   selector becomes a shared capability claim.

## Verification

Keep the repository checks and add behavior-level browser coverage during
implementation. Exercise the existing TUI suite after shared behavior moves.
Use deterministic fixtures for write refusal, lost answers, late responses,
two-tab isolation, hidden-tab read progress and Agent freshness.

Every new interactive feature updates the shared capability catalog and both
surface entries together. Keep an explicit TUI/Web acceptance result per
capability at the same source revision. Intermediate commits may be incomplete;
release completion requires the [synchronized journeys](../docs/interactive.md#synchronized-acceptance)
in both surfaces. Desktop/Mobile source observations do not count as that evidence.

Verify that a bare unauthenticated local request and an untrusted website
cannot read or send as the identity; reject invalid Host/Origin, forged
state-changing requests and credentials from an earlier process. Inspect
responses, browser storage and logs for key/auth-tag leakage. Render hostile
message/profile/error fixtures and prove they remain inert.

Use a real relay for the product's interaction loop, with stored-event and
recipient-authenticated readback. Keep screenshots, protocol evidence and
CI results separate. A static wireframe, mock response or source inspection
cannot establish functional parity.
