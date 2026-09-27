# Find and resume a conversation

Status: M2–M4 are implemented in the TUI. Release acceptance is tracked in
the workspace evidence file `WORK_LOGS/BUZZX_CONTEXT_ACCEPTANCE_LOG.md`.
This expands the accepted complete-message reading direction into one coherent
TUI release. Its baseline is merged commit 88595b9. The reader remains a
component specified in [tui-reading.md](tui-reading.md).

The six modules are shared TUI/Web capabilities under
[interactive.md](interactive.md). Their functional rules and limits apply to
both; the keys and cell budgets here specify TUI presentation. Browser entry
points and return controls are in [web.md](web.md#search-inspect-and-resume).

## Outcome

A terminal user returns to a busy workspace, finds an earlier decision with
only a channel name, author or remembered phrase, reads the surrounding work,
and continues the correct discussion without losing their original place.

The release must support this complete path:

```text diagram
Current work -> Find conversation or search messages -> Open exact context
             -> Read older/newer discussion -> Read complete result
             -> Reply to the intended event -> Return to original work
```

This is one release with six functional modules and four delivery milestones.
A searchable list, a reader, or a paging helper alone does not satisfy it.
Existing Inbox, mention resolution, thread view and visual language are its
foundation, not features to rebuild. It advances the terminal conversation
purpose in [core.md](../core.md); it adds no Agent control or task completion
semantics.

## Evidence and users

At the baseline, channel history reads a bounded latest page, the focused
thread reads root plus at most 500 replies, and the TUI has no search entry.
A source reference is the [history/thread client](../src/client.rs); the
current key surface and bounded thread behavior are in [tui-use.md](tui-use.md).
A real-binary fake-relay reproduction at 40x10 also shows that the end of a
60-line message is unreachable with current event-navigation keys.

The relay's existing NIP-50 search is used by Desktop, Mobile and the Buzz CLI.
A read-only CLI search on the deployed relay returned five requested results
in this design session. This confirms basic search availability, not all
filter combinations, complete results, pagination or authorization cases.

Primary scenarios:

- A developer remembers a decision but not its conversation or exact date.
- A person returns to a busy channel and needs the discussion before a reply.
- A person reads a long Agent report and asks a follow-up about its conclusion.
- A terminal user has a draft open and must consult another discussion safely.

Frequency and latency improvements are hypotheses until tested with users.
The capability failures above are reproduced or source-confirmed facts.

## Why this release

A reader-only release fixes one reproduced defect but still leaves old work
outside the loaded window unreachable. A broad workspace-management release
would add tasks and Agent controls beyond this client's conversation purpose.
This release follows one existing user job across discovery, reading and reply.
That provides a larger useful outcome without a separate management surface.

Prefer a dedicated search/context journey over replacing the main timeline
with filtered rows: the user can tell whether they are looking at a search
sample or their normal conversation, and returning has one clear destination.
The cost is explicit return state, addressed by C6 and the release acceptance.

## Scope and six modules

### C1. Find a conversation

Enhance the existing c picker with a local name filter, entered with `/` while
in the picker. It searches the accessible, listed channels and DMs already
known to the session; it does not discover or join new channels. Preserve the
active Inbox filter and show it beside the name query, so an Unread filter
cannot look like a failed global directory search.

Rank exact, prefix and substring matches, retaining the existing stable order
within each group. Do not reshuffle because a live message arrives. Empty
query restores the same list. Enter from text entry applies the filter and
focuses results; j/k then select, Enter opens, Esc returns one level. Escape
from query editing restores the previous applied query. Scope, name query,
selected identity and scroll position survive the inspection journey.

A nonempty draft keeps the existing conversation-switch guard. Filtering and
previewing cannot discard it. Labels may shorten; unread and unknown signals
retain their reserved space. No match and an incompletely loaded roster are
different states. Fullscreen on small terminals, as the current picker is.

### C2. Search messages

`/` in channel or thread navigation opens full-screen message search. Start
with the current conversation as the explicit scope, including when the user
came from a thread. The user can switch to all accessible listed conversations
or choose one conversation. Search stays on the current relay and identity.

While composing, Ctrl+f suspends the composer and opens this same search.
Preserve draft text, reply/edit target, cursor and composing mode for return.
This is an explicit new entry: ordinary Esc can clear a channel reply target
before leaving composition, so it cannot serve this job. `/` stays literal
text in the composer. Expose Ctrl+f in composer help. Pending writes still
block this entry under the existing navigation guard.

Use separate, visible controls for scope, author and time range. Author
selection resolves to a pubkey and distinguishes duplicate names with a short
key; display text alone is never an identity filter.
The author picker uses known accessible participant profiles and accepts an
exact pubkey; an incomplete list is labeled as such. It does not require a new
global people directory or infer an identity from a partial display name.
Time presets are All,
Past 7 days and Past 30 days, relative to the time the user submits the query.
Do not introduce a query-operator language in this release. Search text uses
the existing relay search semantics; do not promise substring or fuzzy matching
for message bodies merely because conversation-name filtering supports it.

Text entry is its own mode: all printable characters are query text, Enter
submits, Esc restores the last applied query/results. In result navigation,
j/k move selection, Enter opens context, `/` edits the query, f opens the
filter form, ? opens help, and Esc returns to the journey origin. Filters
apply only on explicit submission. No network request is needed per keystroke.
An empty keyword can search by a selected author; neither keyword nor author
shows an invitation to enter one, not a load of the entire archive.

Each result shows conversation, author, time, a matching text excerpt and any
known thread relationship. Identity is the event id; selection never follows
an array index after replacement. Keep relay relevance order for keyword
search; author-only search is newest first. Do not apply live re-ranking while
a user chooses a result. A later query's response cannot be overwritten by an
earlier request. A local row filter must not be presented as relay search.

Request up to 50 results. Reaching that bound shows `Top 50; narrow filters`,
not a total count. A shorter response is `N results returned`, not proof that
all possible matches have been found. Search result pagination is deliberately
not promised: the existing HTTP search bridge may expose only its first
bounded relevance page. History pagination below is a different capability.
If a requested scope/filter is unsupported, report it and retain the query;
do not silently broaden it. In all-accessible scope, hide inaccessible or
unlisted contexts according to the current conversation visibility contract.
Search the supported textual message kinds, not Agent observer records,
system notices or private configuration. If visibility filtering removes
returned hits, label the remaining list as bounded rather than claiming that
the visible count exhausts the search space.

Loading, no returned matches, unsupported search, failed read and bounded
results are distinct. Preserve prior results on retry failure, explicitly
marked as from the previous successful query. Edited/deleted results cannot
be treated as authoritative until the current event and overlays are resolved.
Do not show deleted or revoked content once that fact is known.

### C3. Open the result in context

Enter opens a fullscreen context timeline at the exact hit. Retain a return
record for the applied search query, filters, selected event and list offset.
The context view labels its conversation and `Search context` origin. It is
not the ordinary latest-message view, and it does not move the original
workspace selection or discard a draft just to inspect a hit.

Resolve the hit's current accessible event identity, then read surrounding
conversation messages: target plus up to 20 earlier and 20 later rows initially.
Resolve existing edit, deletion, reaction and root/parent semantics. Pin the
hit as the initial action target and show an explicit selection; a same-author
or same-second neighbor cannot stand in for it. If the hit no longer exists,
show unavailable/deleted and back; never silently land at the channel bottom.

j/k move event focus consistently at all widths in this context surface.
`t` opens the focused row's containing thread, `v` opens its complete text,
and Enter composes a reply using the normal target/mention/write rules. The
thread root stays available as an explicit destination. A result that is a
reply opens context around that reply and focuses it again when entering its
thread; it must not turn into a top-level reply by accident.

Context composition uses the thread composer's one-step Esc behavior: return
to context navigation with text and target retained. With that local draft,
i or Enter resumes the same draft; opening a different target or leaving the
context is blocked until it is sent or cleared. When a suspended origin draft
blocks a new reply, the separate C6 guard applies before a composer opens.

The context timeline can page in either direction through C4. Esc returns to
the same search result. Thread Esc returns to this context, not to an unrelated
channel view. Search failure, missing root and membership revocation all keep
a usable route back. Context reading never implies complete archive coverage.

### C4. Read earlier and later history

Channel/DM timelines, search context and focused threads support bounded
incremental history reads. Reaching a loaded boundary with an event-navigation
step requests one adjacent page; retain existing visible rows, event focus and
body position while loading. A failure is retryable at that boundary without
clearing the successful window. Repeated keys while loading do not enqueue
identical requests. Decorations/loading notices are not selectable events.

The product page target is 50 confirmed timeline rows, plus any exact anchor
needed for continuity. Filter auxiliary kinds separately so edits and reactions
do not consume that target. Preserve event identity and deterministic order,
deduplicate overlap, and resolve overlays for newly loaded rows. Reading past
the former 500-reply cap must be possible where the relay can supply pages.

Do not skip a timestamp boundary with `oldest_second - 1` until all events at
that second are accounted for. Validate the deployed query's order, inclusive
bounds and limits before selecting a paging strategy. If stable traversal
cannot pass a saturated same-second boundary, report `History limit reached`
with the visible gap and keep the readable window; never claim `Beginning`
or complete coverage. Internal search-service page fields are not proof that
clients have a usable cursor.

Bound memory with a sliding history window (target: at most 2,000 loaded rows
for the active history view, excluding the separately retained thread root).
Discard only distant cached pages, preserve the current/return anchors by
identity, and reload evicted text when needed. Cache capacity is not a limit
on the conversation's total history. Re-read failures when returning to an
evicted anchor show a recoverable error rather than replacing the destination.

`G` in a timeline/context/thread explicitly requests the newest window and
focuses its newest valid event; if that read fails, keep the prior position.
This intentionally extends the current newest-loaded behavior. In the reader,
G remains end-of-this-message. Automatic live-follow applies only when the
ordinary timeline is already following latest; it never pulls a search context,
old-history position or reader to the bottom.

Keep loading earlier/later, known boundary, bounded/partial, stale and failure
states distinct. `Beginning` requires evidence that the boundary is exhausted;
fewer visible rows after client filtering alone is not that evidence. The
root/reply association contract still excludes unrelated reference events.

### C5. Read the complete message

Use [tui-reading.md](tui-reading.md) as the single behavioral specification.
Its proposed v entry, per-line scrolling, anchored resize, metadata, live-edit
and deletion rules now apply from ordinary, search-context and thread views.
This closes the reproduced 60-line failure and is a required part of this
release, not the release by itself.

### C6. Resume work without changing intent

The journey has a fixed, meaningful return chain:

```text diagram
Original work -> Search -> Context -> Thread -> Reader
```

Steps may be skipped. Esc unwinds one actual visited view. Searching again
replaces the journey's search state rather than creating unlimited nested
searches. Save identities, applied filters, logical text anchors and focus,
not only line numbers or list indexes. Session-only return state is enough;
this release adds no cross-restart history, bookmarks or persisted draft system.

Entering search, context or reader can inspect text while a draft is retained.
Threads opened inside this inspection journey also permit reading while the
origin composer is suspended; writes still face the draft guard. This extends
inspection without changing ordinary channel-to-thread draft restrictions.
Any action that would change its destination is guarded before it changes view.
If an existing nonempty draft would be replaced or retargeted, preserve it,
remain in the read view and show `Draft kept; Esc back`. Reuse the current
per-conversation draft ownership; do not overwrite another conversation's
draft when replying from a cross-conversation search result. Pending writes
retain their existing navigation guard. Unknown writes are not resumable drafts
and are never automatically sent again.

With no blocking draft, reply opens the existing composer in the context's
conversation. A reply to a nested event keeps both root and parent, and
explicit mentions retain signed recipient semantics. Send confirmation or
failure appears in that view. After replying, the user can return through the
same search and back to the original channel, focused event and Inbox filter.
Draft cancellation follows the originating composer's existing rules.

Search previews, context, threads and reader do not mark an entire channel
read. New events behind these views remain unread under covered-timeline
rules. Only presenting the ordinary channel's latest timeline can advance its
existing frontier; pressing G in search context is not that presentation.
Do not retract a frontier that already advanced before the journey began.

If a return event is deleted, select the nearest surviving neighbor and show
that the original target was deleted. If its channel becomes inaccessible,
clear revoked content and return to accessible navigation with an explicit
state. Preserve unrelated drafts without showing revoked content.

## Shared UI and minimum budgets

Reuse [revision 5](tui-visual.md). All new views are single-column fullscreen,
with context at the top, content in the middle, and actions/state at the bottom.
No additional persistent sidebar, split-thread panel or card language is added.
The existing wide channel sidebar remains as defined by the current layout.

At 24x6, search uses header, applied query/scope summary, two result/context
rows, actions and state. A filter form replaces results while editing. Show
one selected result's author and excerpt over those two rows when necessary;
full conversation/name/time/filter details remain in help. The reader's
six-row budget stays authoritative. Result labels must not push out the action,
error, coverage or reply target. Higher widths increase readable text before
adding decoration; use the same key semantics in all new views.

The [release map](assets/tui-context.html) shows the integrated journey and
milestones; the linked reader simulation illustrates C5 in detail. Both are
review artifacts, not running product features.

## Delivery milestones and dependency order

| Milestone | Includes | Exit demo |
| --- | --- | --- |
| M1: A stable reading destination | C5 and C6 origin/return foundation | Read the 60-line conclusion, return to the same target, and refuse retargeting a retained draft |
| M2: Find the discussion | C1, C2 and C3 initial context | Search outside the loaded window, open the exact hit and its thread, return to the same result |
| M3: Continue beyond loaded history | C4 across all three history surfaces | Cross the old history/thread caps with no duplicate/missing boundary events or focus jump |
| M4: Complete the collaboration journey | C6 integration and cross-surface acceptance | Search an old decision, read full context, mention/reply, then resume original work |

M1 can be reviewed independently, but the Epic remains open until M4 and the
release acceptance below pass. C2/C4 require an early, read-only relay contract
check for filter conjunction, access, ordering and pagination bounds. If a
required contract is unavailable, surface a specific dependency; do not create
an undocumented relay extension or declare the reduced slice the whole release.

One product owner keeps the shared state rules; implementation can split by
milestone with a final integration review. These are dependency-sized work
packages, not promises of dates or staffing capacity.

## Release acceptance

Use known identities, channels, event ids and a seeded reference dataset.
Record candidate commit, binary hash, initial/final screen, stored reply tags
and read markers. One screenshot or unit suite is insufficient.

| Case | Scenario and required outcome |
| --- | --- |
| E1 | In 50 listed conversations, find a named channel/DM while retaining the active Inbox filter; no hidden or inaccessible conversation appears |
| E2 | Across accessible conversations, find a known old phrase outside the loaded history; scope, author and 7/30-day filters return the intended identities/time range |
| E3 | Open a hit among 1,200 channel messages; the exact event is selected, earlier/later context is available, Esc restores query/filters/result offset |
| E4 | Read a 700-reply thread beyond the old 500-reply bound; retain root and focused reply, avoid unrelated referenced events |
| E5 | At least 120 events share a second across a page boundary; traversal has no duplicate/missing event against the reference set, or explicitly exposes an untraversable limit |
| E6 | Read a 60-line report to its final decision at 40x10 and 24x6, including code indentation, CJK, emoji and a long URL |
| E7 | From an old nested hit, reply with an explicit mention; relay stores the exact conversation/root/parent/recipient and shows its actual write outcome |
| E8 | Enter search with Ctrl+f while composing a reply or edit; inspect another conversation and attempt E7 with original/target drafts; return restores text, target, cursor and mode, with no retarget, overwrite or accidental publish |
| E9 | Receive new messages, edits and deletion during the journey; no stolen focus, stale deleted body or shifted write target |
| E10 | Observe markers before/during/after search, context, thread and reader; previews do not clear the channel's unread backlog |
| E11 | Timeout, reconnect, unsupported search, paging saturation and lost access remain distinguishable; read retry never becomes a write retry |
| E12 | Run the end-to-end journey at 120x30, 80x12, 79x12, 40x10, 24x6 and a too-small round trip; validate dark/light defaults and NO_COLOR |

Task success is finding the seeded decision, reading its conclusion, publishing
exactly one intended reply and returning to the original work. Use five
representative user runs as a usability check: record completion, wrong-target
attempts, lost drafts and lost-place reports. Target all five completing, zero
wrong-target writes and zero lost drafts. These are proposed acceptance targets,
not measured results or a statistically established adoption forecast.

## Boundaries

This release excludes semantic/AI search, automatic summaries, task or approval
management, Agent controls, channel creation/joining, new media rendering,
bookmarks, cross-relay search and a custom query language. It does not promise
an exhaustive archive search result count. It must not remove the existing
partial/unknown states to make the larger feature appear complete.
