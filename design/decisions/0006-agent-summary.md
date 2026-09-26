# Decision: 0006 - Read-only owner Agent summaries

## Status

accepted

Implemented on this branch as a read-only overlay; the product contract is
[docs/tui-use.md](../../docs/tui-use.md#agents-overview), and what was checked
against a deployed relay is recorded there. Partially supersedes
[decision 0003](0003-owner-plane-out-of-scope.md).

## Context

Terminal users now explicitly need to see their owned Agents and actual work.
The earlier exclusion prevented this use case. Presence and typing cannot
prove that an Agent is executing a turn. Desktop already derives work from
owner observer events; Mobile separates channel bot typing and an encrypted
observer activity view. Source references are below, not deployment evidence.

## Decision

Permit the current owner's verified roster and a minimal in-memory projection
of existing authenticated owner-private observer events. Reuse upstream
ownership and encryption semantics, the existing identity and live transport.
Do not introduce a new event kind, public status broadcast, polling service,
persistent run database or Agent-management abstraction.

Keep only identity, turn/context references, terminal state and freshness
needed by the [product contract](../../docs/tui-use.md#agents-overview).
Discard other observer fields after decoding; do not retain raw frames or
transcripts. Key activity by producer/session and turn identity as provided
by upstream, not a process-local sequence treated as globally monotonic.
Verify sender, intended owner and context access before exposing a summary.
Bound retained state and clear it on identity changes or loss of access.

This permits reception of kind 24200 solely for the summary. It permits no
control frames, task cancellation, tool logs or owner configuration. Build
must verify roster discovery and observer recovery on the deployed version
before claiming the complete capability. Inability to read private data from
an Agent identity is not proof the owner's roster is empty.

## Desktop and Mobile comparison

Reference baseline: block/buzz revision
`93114c9c65138397de39729fde0a816eb9f314ab`.

- [Desktop managed rows](https://github.com/block/buzz/blob/93114c9c65138397de39729fde0a816eb9f314ab/desktop/src/features/agents/ui/ManagedAgentRow.tsx)
  show Agent identity and active working channels. Adopt that direct path;
  omit process, model, configuration and log controls.
- [Desktop working signal](https://github.com/block/buzz/blob/93114c9c65138397de39729fde0a816eb9f314ab/desktop/src/features/agents/agentWorkingSignal.ts)
  aggregates observer turns with typing fallback. Preserve source distinction
  in TUI wording rather than claiming both prove execution.
- [Mobile working bots](https://github.com/block/buzz/blob/93114c9c65138397de39729fde0a816eb9f314ab/mobile/lib/features/channels/agent_activity/working_bots_provider.dart)
  derives a channel badge from typing intersected with bot members. That badge
  is not an owned roster or a global execution inventory.
- [Mobile observer subscription](https://github.com/block/buzz/blob/93114c9c65138397de39729fde0a816eb9f314ab/mobile/lib/features/channels/agent_activity/observer_subscription.dart)
  has owner-scoped encrypted observation and channel-scoped transcript views.
  Reuse the authorization boundary; do not copy its transcript buffer or UI.

These are source comparisons, not live Desktop/Mobile walkthroughs. The TUI
uses one overlay and one detail step instead of copying either client's full
Agent management surface.

## Alternatives considered

**Bot roster plus typing only.** Small, but cannot answer either complete
ownership or actual execution. Keep typing's weaker label without treating
it as delivery of this requirement.

**Full owner console.** Adds control, history, models, logs and task management.
None is necessary to find an Agent and return to its working conversation.

**New public working protocol.** Requires upstream changes and risks exposing
owner-private activity. Use the existing private source instead.
