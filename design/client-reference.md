# Desktop and Mobile reference

Status: source comparison for the synchronized TUI/Web product design.
This document records observations and design choices, not app-runtime
acceptance. No Desktop or Mobile session was exercised for this comparison.

## Baselines

- buzzx: [main 4d3fe9d](https://github.com/suraciii/buzzx/tree/4d3fe9d2f3825b2440679b5282f7ea13f2525284), including complete reading and context recovery.
- Buzz Desktop and Mobile: [main b0d6fb8ad](https://github.com/block/buzz/tree/b0d6fb8ad27f6f255a5044e49ed0a59e11542914), fetched for this design.
- buzzx still pins Buzz crates at `93114c9c6` in [Cargo.toml](../Cargo.toml).
  A newer reference-client feature does not prove that the pinned SDK or the
  deployed relay supports its protocol. This change does not upgrade either.

Buzz's [vision][vision] treats conversation, search and identity as a shared
workspace across surfaces. buzzx retains its [client boundary](../core.md)
and applies that consistency to its two interactive interfaces through the
[shared capability catalog](../docs/interactive.md).

## What the source shows and what buzzx takes from it

| Area | Desktop source | Mobile source | Decision for both buzzx surfaces |
| --- | --- | --- | --- |
| Discovery and search | [Scope chip][desktop-scope] and [topbar results][desktop-search] expose context | [Search provider][mobile-search] separates message, user and channel results | Keep conversation-name filtering distinct from message search; show scope explicitly |
| Exact navigation | [Hit destination][desktop-hit] carries message and thread identities | [Result navigation][mobile-hit] opens a forum post or channel page | Require the exact hit in buzzx context; opening only the channel is insufficient |
| Threads and history | [Thread loading][desktop-thread] traverses cursor pages and reconciles arrivals | [Thread provider][mobile-thread] scans cursor pages and refreshes on reconnect | Preserve complete user traversal, live overlays and explicit partial/error states |
| Reading position | [Anchored scroll][desktop-scroll] tracks event identity and latest state | [Thread list][mobile-list] keeps root first and a latest anchor | Keep old reading position during arrivals; expose an explicit latest action |
| Thread layout | [Panel layout][desktop-layout] supports standalone and split presentation | [Thread detail][mobile-detail] uses a dedicated page | Use one focused content region with Back; a third Web column is unnecessary |
| Composition | [Draft store][desktop-drafts] binds content, channel and selection | [Draft lifecycle][mobile-drafts] binds draft identity and guards late restoration | Preserve destination and current text across inspection; do not restore into a newer draft |
| Mention identity | [Draft mention references][desktop-mentions] preserve explicit identities and ambiguity | [Recipient builder][mobile-mentions] combines explicit identities with DM recipients | Reuse buzzx's current explicit-only recipient policy and plain-text correction |
| Attention and read state | [Inbox grouping][desktop-inbox] distinguishes thread and message read contexts | [Inbox read state][mobile-inbox] keeps unrelated channel activity unread | Reuse buzzx's channel frontier and honest unknown state; do not copy the larger Inbox model |
| Message content | [Markdown renderer][desktop-content] includes media and code presentation | [Message content][mobile-content] includes markdown, media and attachments | Make every existing text/metadata field readable; media operations require a separate shared capability |

## Differences that matter

The Mobile search path inspected here passes only a channel to ordinary stream
result navigation; it does not pass the hit id in that call. Desktop's
resolver does carry that id. buzzx already defines a stronger exact-context
contract, so both TUI and Web retain it rather than reproducing the weaker
path. This is a source-level observation, not a claim about a tested mobile
session or every mobile navigation route.

Both reference clients have richer draft models. Desktop persists drafts in
localStorage; Mobile restores draft identity and mention bindings through its
draft provider. buzzx keeps its current session-only drafts. Safe return and
late-result protection are adopted without adding a persistent draft product.

Mobile adds current DM participants to signed recipients automatically.
buzzx's existing [mention rules](../docs/tui-use.md#mentions-when-sending)
use explicit recipients. Web must use the buzzx rule too. No automatic
invitation, new mention editor or edited-message notification promise is
introduced by copying a richer composer's appearance.

Latest Mobile thread reads send `thread_cursor` and `thread_cursor_id`;
Desktop receives a cursor-bearing thread response. These are newer contracts
than the older reference used for the first Web draft. Reuse the current
buzzx history implementation and its coverage limits. A future cursor upgrade
must first prove deployed support and then apply to TUI and Web together.

Desktop/Mobile Inbox items include thread/message read contexts and actions
beyond buzzx's conversation list. Keep the existing `All`, `Unread`, `For you`
filters and channel read markers in both buzzx surfaces. A familiar label
must not silently introduce a second definition of unread or task completion.

## Chosen scope

The design adopts clear search scope, event-based navigation, stable reading
position, visible composition targets and an explicit return path. The newest
TUI's search, history and reader become required Web capability in the same
release. The browser adapts these to forms, buttons and scrolling; the TUI
retains its terminal controls.

Two alternatives were considered: copy the full Desktop/Mobile workbench,
or keep the original Web snapshot and postpone the newer TUI functions. The
first expands beyond buzzx's conversation-client boundary; the second already
violates the requested capability synchronization. The shared catalog and
surface-specific interaction specifications preserve the full current
conversation workflow with one product scope.

[vision]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/VISION.md
[desktop-scope]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/desktop/src/features/search/ui/SearchScopeControls.tsx
[desktop-search]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/desktop/src/features/search/ui/TopbarSearch.tsx
[desktop-hit]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/desktop/src/app/navigation/resolveSearchHitDestination.ts
[desktop-thread]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/desktop/src/features/messages/useThreadReplies.ts
[desktop-scroll]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/desktop/src/features/messages/ui/useAnchoredScroll.ts
[desktop-layout]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/desktop/src/features/channels/lib/threadPanelLayout.ts
[desktop-drafts]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/desktop/src/features/messages/lib/useDrafts.ts
[desktop-mentions]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/desktop/src/features/messages/lib/draftMentionRefs.ts
[desktop-inbox]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/desktop/src/features/home/lib/inbox.ts
[desktop-content]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/desktop/src/shared/ui/markdown.tsx
[mobile-search]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/mobile/lib/features/search/search_provider.dart
[mobile-hit]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/mobile/lib/features/search/search_page.dart#L941
[mobile-thread]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/mobile/lib/features/channels/thread_replies_provider.dart
[mobile-list]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/mobile/lib/features/channels/thread_detail_page/message_list.dart
[mobile-detail]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/mobile/lib/features/channels/thread_detail_page.dart
[mobile-drafts]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/mobile/lib/features/channels/compose_bar/draft_lifecycle.dart
[mobile-mentions]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/mobile/lib/features/channels/message_mention_pubkeys.dart
[mobile-inbox]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/mobile/lib/features/activity/inbox_read_state.dart
[mobile-content]: https://github.com/block/buzz/blob/b0d6fb8ad27f6f255a5044e49ed0a59e11542914/mobile/lib/features/channels/message_content.dart
