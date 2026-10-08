//! The live session: the subscriptions, the reconnect, and the `ChatEvent`
//! stream the TUI renders. The command pump translates one `SessionCommand`
//! into one core call and one `ChatEvent`; the relay operations themselves
//! live in `client.rs`. The contract is design/shared-core.md.

use std::collections::{HashMap, VecDeque};

use nostr::Event;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::client::{Client, WriteOutcome, event_id};
use crate::config::Resolved;
use crate::content;
use crate::failure::{Category, Failure};

use crate::sub::{self, SubControl};

/// How many history events one open fetches.
const HISTORY_LIMIT: u64 = 100;
/// How long a read waits before it is published. Reading a busy conversation
/// otherwise writes a marker per message; the newest picture is the one worth
/// sending.
const READ_PUBLISH_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// The loaded surface a history page extends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySurface {
    Channel,
    Context,
    Thread,
}
/// Lifecycle operation named in write responses. The relay is authoritative;
/// the session never turns a stored event into optimistic local state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelOp {
    Edit,
    Archive,
    Unarchive,
}

/// A membership mutation. One command is emitted for each target identity so
/// a batch can stop between targets when its generation changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberOp {
    Add {
        pubkey: String,
        role: buzz_core::channel::MemberRole,
    },
    SetRole {
        pubkey: String,
        role: buzz_core::channel::MemberRole,
    },
    Remove {
        pubkey: String,
    },
    Leave,
}

/// What the transport side tells the UI. Every mutation the UI renders
/// arrives as one of these.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// Events from an older relay generation are dropped by the UI.
    Generation {
        generation: u64,
        event: Box<ChatEvent>,
    },
    /// The old relay is no longer visible while the new one authenticates.
    CommunitySwitching {
        generation: u64,
        id: Option<String>,
        name: String,
        relay_url: String,
    },
    Connected,
    Disconnected(String),
    /// `complete` is false when the relay could not describe every row.
    Channels(crate::client::Roster),
    /// A channel's history, oldest first. Replaces the loaded rows.
    History {
        channel: Uuid,
        events: Vec<Event>,
    },
    /// A channel history request failed after its live feed started. The UI
    /// must leave loading and keep the read result unknown so the user can
    /// retry by reopening it.
    HistoryFailed {
        channel: Uuid,
        reason: String,
    },
    /// Display names for authors the UI has seen.
    Profiles(Vec<(String, String)>),
    /// One live timeline event for a subscribed channel.
    Timeline {
        channel: Uuid,
        event: Event,
    },
    /// One live timeline event for a listed conversation the Inbox feed
    /// watches. A conversation on screen is owned by its own `Timeline` feed,
    /// which also renders it.
    InboxTimeline {
        channel: Uuid,
        event: Event,
    },
    /// One bounded thread read: the root first, then its replies, oldest
    /// first. `partial` is true when the reply query reached its bound, so the
    /// view must not present the result as complete history.
    Thread {
        channel: Uuid,
        root: String,
        request: u64,
        events: Vec<Event>,
        partial: bool,
    },
    /// A bounded relay search result, with the request token that owns it.
    Search {
        request: u64,
        events: Vec<Event>,
    },
    SearchFailed {
        request: u64,
        reason: String,
    },
    /// A context window around one exact event.
    Context {
        channel: Uuid,
        target: String,
        request: u64,
        events: Vec<Event>,
        before_complete: bool,
        after_complete: bool,
    },
    ContextFailed {
        channel: Uuid,
        target: String,
        request: u64,
        reason: String,
    },
    /// One bounded page for a channel, context, or thread surface.
    /// `saturated` is the raw answer's cut-page evidence: true means more
    /// rows may exist in the page's direction, false is the boundary.
    HistoryPage {
        surface: HistorySurface,
        channel: Uuid,
        root: Option<String>,
        request: u64,
        direction: crate::client::HistoryDirection,
        events: Vec<Event>,
        saturated: bool,
    },
    HistoryPageFailed {
        surface: HistorySurface,
        channel: Uuid,
        root: Option<String>,
        request: u64,
        direction: crate::client::HistoryDirection,
        reason: String,
    },
    /// The newest window of one surface, fetched for `G`. Replaces the
    /// surface's loaded rows; the app focuses its newest valid event and
    /// keeps its return state. `saturated` says the window was cut by its
    /// bound, not that the archive ends at the window's oldest row.
    NewestWindow {
        surface: HistorySurface,
        channel: Uuid,
        root: Option<String>,
        request: u64,
        events: Vec<Event>,
        saturated: bool,
    },
    /// The newest-window read failed: the surface keeps its prior rows and
    /// position, per the contract that a failed read never moves the user.
    NewestWindowFailed {
        surface: HistorySurface,
        channel: Uuid,
        root: Option<String>,
        request: u64,
        reason: String,
    },
    /// A thread read failed: the root is missing or inaccessible, or the query
    /// did not answer. A view that already holds rows keeps them.
    ThreadFailed {
        channel: Uuid,
        root: String,
        request: u64,
        reason: String,
    },
    /// The relay closed one conversation's Inbox feed: new messages there are
    /// no longer observed, so its unread state stops being trustworthy.
    InboxClosed {
        channel: Uuid,
        reason: String,
    },
    /// The identity's read frontier, merged from every marker slot it owns.
    /// `complete` is false when the lookup or a slot's decode failed: a
    /// context that is missing is then unknown, not absent.
    ReadState {
        contexts: HashMap<String, u64>,
        complete: bool,
    },
    /// Catch-up for one conversation: what it holds at or after the read
    /// frontier. `complete` is false when the query failed or was truncated,
    /// which leaves coverage incomplete rather than the conversation read.
    CatchUp {
        channel: Uuid,
        events: Vec<Event>,
        complete: bool,
    },
    /// The newest known message of a conversation that has no marker yet: the
    /// local baseline to track new messages from. `complete` is false when the
    /// query failed. `latest` is None for an empty conversation, which starts
    /// tracking without inventing unread.
    Seed {
        channel: Uuid,
        latest: Option<Event>,
        complete: bool,
    },
    /// The answer to publishing this identity's read state.
    ReadPublished {
        ok: bool,
        reason: String,
    },
    /// One live auxiliary event: a reaction, edit, or deletion.
    Overlay(Event),
    /// One identity is composing in one channel. Ephemeral: it arrives on the
    /// live connection only and is never part of the timeline. `at` is the
    /// indicator's own event time, which is what a later message is compared
    /// against.
    Typing {
        channel: Uuid,
        pubkey: String,
        at: u64,
    },
    /// A typing indicator scoped to one thread head (`e` tag). Kept separate
    /// from the channel-wide event so existing callers remain source-compatible.
    TypingScoped {
        channel: Uuid,
        pubkey: String,
        head: String,
        at: u64,
    },
    /// The relay closed the typing feed of one channel. The channel itself may
    /// still be open; only its indicators stopped.
    TypingClosed {
        channel: Uuid,
        reason: String,
    },
    /// The relay closed a channel subscription: membership was lost.
    ChannelGone {
        channel: Uuid,
        reason: String,
    },
    /// The answer to an owned-roster read. The four loads are separate states:
    /// an empty roster and a roster this identity may not read are not the
    /// same answer.
    Agents(crate::agents::Load),
    /// One owner-private observer frame, already reduced to the fields the
    /// summary keeps.
    ObserverFrame(crate::agents::Frame),
    /// The relay established the observer feed on this connection. Only after
    /// this point is the absence of a frame evidence that no turn runs.
    ObserverReady,
    /// The relay closed the observer feed: with no feed, a quiet Agent is
    /// unknown rather than idle. The session keeps running.
    ObserverClosed {
        reason: String,
    },
    /// A relay notice or a publish answer worth showing on the status line.
    Status(String),
    /// A send the relay never saw: the draft's mention text did not resolve to
    /// current members, or the membership read failed. Nothing was published.
    /// The block carries its own kind, summary and correction details.
    MentionBlocked {
        local: String,
        channel: Uuid,
        block: crate::mentions::Block,
    },
    /// The candidate identities of one conversation, read for the composer's
    /// `@` picker. `request` is the token the picker asked with, so a late
    /// answer for a superseded query is dropped rather than shown.
    MentionCandidates {
        channel: Uuid,
        request: u64,
        result: Result<Vec<crate::mentions::Candidate>, String>,
    },
    /// The answer to a channel-creation request. `draft` is what the form
    /// submitted, so the result can name the channel it describes.
    ChannelCreated {
        request: u64,
        draft: crate::client::ChannelDraft,
        outcome: crate::client::CreateOutcome,
    },
    WriteOk {
        local: String,
        event_id: String,
    },
    WriteFailed {
        local: String,
        reason: String,
    },
    WriteUncertain {
        local: String,
        reason: String,
    },
    /// Result of one channel metadata/archive operation. The UI refreshes
    /// authoritative state after a stored result; it never mutates from this.
    ChannelWriteResult {
        request: u64,
        channel: Uuid,
        op: ChannelOp,
        outcome: crate::client::WriteOutcome,
    },
    /// Authoritative membership readback. Errors remain distinct from an
    /// empty roster.
    Members {
        channel: Uuid,
        request: u64,
        result: Result<crate::client::Members, String>,
    },
    /// Result of one membership mutation in a sequential batch.
    MemberWriteResult {
        request: u64,
        channel: Uuid,
        op: MemberOp,
        outcome: crate::client::WriteOutcome,
    },
    /// A queued community switch stopped the unsubmitted targets in a batch.
    MemberBatchCancelled {
        request: u64,
        channel: Uuid,
        remaining: Vec<String>,
    },
    /// Candidate identities for the member picker.
    MemberCandidates {
        channel: Uuid,
        request: u64,
        result: Result<Vec<crate::mentions::Candidate>, String>,
    },
}

/// What the UI asks the session to do.
#[derive(Debug, Clone)]
pub enum SessionCommand {
    /// Stop the current subscriptions and bind the single active connection
    /// to another saved profile.
    SwitchCommunity(Resolved),
    LoadChannels,
    /// Agents view opens, so a session that never opens it never reads it.
    LoadAgents,
    /// Fetch history, subscribe live, and backfill overlays for one channel.
    OpenChannel(Uuid),
    /// Read one conversation's thread: its root and a bounded page of replies.
    /// Asked for when the thread view opens or a failed read is retried.
    OpenThread {
        channel: Uuid,
        root: String,
        request: u64,
    },
    /// Search loaded or relay history without blocking the UI.
    Search {
        request: crate::client::SearchRequest,
        token: u64,
    },
    OpenContext {
        channel: Uuid,
        target: String,
        request: u64,
    },
    HistoryPage {
        surface: HistorySurface,
        channel: Uuid,
        root: Option<String>,
        request: u64,
        direction: crate::client::HistoryDirection,
        cursor: u64,
    },
    /// The newest window of one surface, behind `G`. The surface's loaded
    /// rows are replaced; the app keeps its return state and focuses the
    /// newest valid row.
    NewestWindow {
        surface: HistorySurface,
        channel: Uuid,
        /// The thread root, for `HistorySurface::Thread`.
        root: Option<String>,
        request: u64,
    },
    /// Extend the selected conversation's auxiliary feed with a live row
    /// whose id was not part of the HTTP history answer.
    AddAux {
        channel: Uuid,
        ids: Vec<String>,
    },
    /// Resolve display names for authors the UI is missing.
    LoadProfiles(Vec<String>),
    /// Fetch what these conversations hold at or after their read frontier, or
    /// their newest message when they have none yet.
    CatchUp(Vec<crate::client::CatchUp>),
    /// What this terminal has read, by context key, as the shared marker should
    /// carry it. The publisher merges it with what the relay already holds.
    ReadProgress {
        contexts: std::collections::HashMap<String, u64>,
    },
    Send {
        channel: Uuid,
        content: String,
        /// `(root, parent)` thread context, both full hex ids.
        thread: Option<(String, String)>,
        local: String,
        /// The identities the composer inserted from the picker, draft-local.
        /// The preflight still resolves the text itself.
        bindings: Vec<crate::mentions::Binding>,
    },
    /// Read one conversation's candidate identities for the `@` picker.
    MentionCandidates {
        channel: Uuid,
        request: u64,
    },
    /// Create a channel in the active community.
    CreateChannel {
        draft: crate::client::ChannelDraft,
        request: u64,
    },
    /// Update channel name, description and visibility (9002).
    EditChannel {
        channel: Uuid,
        name: String,
        description: String,
        visibility: buzz_core::channel::ChannelVisibility,
        request: u64,
    },
    /// Archive or restore a channel (9002).
    SetArchived {
        channel: Uuid,
        archived: bool,
        request: u64,
    },
    /// Refresh authoritative membership.
    LoadMembers {
        channel: Uuid,
        request: u64,
    },
    /// Search visible profiles, independently of the channel's member roster.
    MemberCandidates {
        channel: Uuid,
        query: String,
        request: u64,
    },
    /// Add several identities, one relay write per identity.
    AddMembers {
        channel: Uuid,
        pubkeys: Vec<String>,
        role: buzz_core::channel::MemberRole,
        request: u64,
    },
    /// Change a member role (9000).
    SetMemberRole {
        channel: Uuid,
        pubkey: String,
        role: buzz_core::channel::MemberRole,
        request: u64,
    },
    /// Remove a member (9001).
    RemoveMember {
        channel: Uuid,
        pubkey: String,
        request: u64,
    },
    /// Leave a channel (9022).
    LeaveChannel {
        channel: Uuid,
        request: u64,
    },
    Edit {
        channel: Uuid,
        target: String,
        content: String,
        local: String,
    },
    Delete {
        channel: Uuid,
        target: String,
        local: String,
    },
    /// React, or remove the identity's own reaction when `remove` names its
    /// event id.
    React {
        target: String,
        emoji: String,
        remove: Option<String>,
        local: String,
    },
    Shutdown,
}

/// The handle the UI holds: one command channel and the first-connection
/// result.
pub struct Session {
    pub commands: mpsc::Sender<SessionCommand>,
    pub started: oneshot::Receiver<Result<(), (i32, String)>>,
    /// Resolves when the command pump has finished, including the read it owes
    /// a quitting session.
    pub finished: oneshot::Receiver<()>,
}

/// Start the session: one WebSocket pump and one command pump, both feeding
/// the same event channel.
pub fn spawn(resolved: &Resolved, events: mpsc::Sender<ChatEvent>) -> Session {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (sub_tx, sub_rx) = mpsc::channel(64);
    let (sub_events_tx, sub_events_rx) = mpsc::channel(256);
    let (started_tx, started_rx) = oneshot::channel();
    let (finished_tx, finished_rx) = oneshot::channel();

    tokio::spawn(forward_sub_events(sub_events_rx, events.clone(), 0));
    spawn_sub_pump(resolved, sub_rx, sub_events_tx, started_tx);

    tokio::spawn(run_command_pump(
        Client::new(resolved),
        resolved
            .community
            .as_ref()
            .map(|community| community.id.clone()),
        cmd_rx,
        sub_tx,
        events,
        finished_tx,
    ));

    Session {
        commands: cmd_tx,
        started: started_rx,
        finished: finished_rx,
    }
}

fn spawn_sub_pump(
    resolved: &Resolved,
    sub_rx: mpsc::Receiver<SubControl>,
    events: mpsc::Sender<ChatEvent>,
    started: oneshot::Sender<Result<(), (i32, String)>>,
) {
    let (_http_url, ws_url) =
        crate::config::split_relay_url(&resolved.http_url).expect("a resolved URL always splits");
    let auth_tag = resolved.auth_tag.as_deref().map(|s| {
        buzz_sdk::nip_oa::parse_auth_tag(s).expect("resolve() verified the tag at startup")
    });
    tokio::spawn(sub::run_ws_pump(
        ws_url,
        resolved.keys.clone(),
        auth_tag,
        sub_rx,
        events,
        started,
    ));
}

async fn forward_sub_events(
    mut source: mpsc::Receiver<ChatEvent>,
    events: mpsc::Sender<ChatEvent>,
    generation: u64,
) {
    while let Some(event) = source.recv().await {
        if events
            .send(ChatEvent::Generation {
                generation,
                event: Box::new(event),
            })
            .await
            .is_err()
        {
            break;
        }
    }
}

/// A malformed thread id never becomes an unthreaded message.
fn thread_ref(thread: &Option<(String, String)>) -> Result<Option<buzz_sdk::ThreadRef>, Failure> {
    let Some((root, parent)) = thread else {
        return Ok(None);
    };
    Ok(Some(buzz_sdk::ThreadRef {
        root_event_id: event_id(root)?,
        parent_event_id: event_id(parent)?,
    }))
}

struct MemberBatch {
    channel: Uuid,
    remaining: VecDeque<String>,
    role: buzz_core::channel::MemberRole,
    request: u64,
}

async fn run_command_pump(
    mut client: Client,
    mut active_community: Option<String>,
    mut commands: mpsc::Receiver<SessionCommand>,
    mut subs: mpsc::Sender<SubControl>,
    events: mpsc::Sender<ChatEvent>,
    finished: oneshot::Sender<()>,
) {
    let mut generation = 0u64;
    let mut pending_read: Option<HashMap<String, u64>> = None;
    let mut due: Option<tokio::time::Instant> = None;
    let mut queued = VecDeque::new();
    let mut member_batch: Option<MemberBatch> = None;
    loop {
        if let Some(batch) = member_batch.as_mut() {
            while let Ok(command) = commands.try_recv() {
                queued.push_back(command);
            }
            if queued.iter().any(|command| {
                matches!(
                    command,
                    SessionCommand::SwitchCommunity(_) | SessionCommand::Shutdown
                )
            }) {
                let batch = member_batch.take().expect("active batch");
                let _ = events
                    .send(ChatEvent::MemberBatchCancelled {
                        request: batch.request,
                        channel: batch.channel,
                        remaining: batch.remaining.into(),
                    })
                    .await;
            } else if let Some(pubkey) = batch.remaining.pop_front() {
                let outcome = client.add_member(batch.channel, &pubkey, batch.role).await;
                let _ = events
                    .send(ChatEvent::MemberWriteResult {
                        request: batch.request,
                        channel: batch.channel,
                        op: MemberOp::Add {
                            pubkey,
                            role: batch.role,
                        },
                        outcome,
                    })
                    .await;
                if batch.remaining.is_empty() {
                    member_batch = None;
                }
                continue;
            } else {
                member_batch = None;
            }
        }
        let command = if let Some(command) = queued.pop_front() {
            Some(command)
        } else {
            match due {
                Some(at) => tokio::select! {
                    command = commands.recv() => command,
                    _ = tokio::time::sleep_until(at) => {
                        due = None;
                        if let Some(contexts) = pending_read.take() {
                            publish_read(&client, &contexts, &events).await;
                        }
                        continue;
                    }
                },
                None => commands.recv().await,
            }
        };
        let Some(command) = command else {
            // The command channel closing is the other way a session ends, and
            // it owes the same flush as a shutdown command.
            flush_read(&client, &mut pending_read, &events).await;
            break;
        };
        match command {
            SessionCommand::SwitchCommunity(resolved) => {
                flush_read(&client, &mut pending_read, &events).await;
                generation = generation.saturating_add(1);
                let community = resolved.community.clone();
                let (sub_tx, sub_rx) = mpsc::channel(64);
                let (sub_events_tx, sub_events_rx) = mpsc::channel(256);
                let (started_tx, started_rx) = oneshot::channel();
                let relay_url = resolved.http_url.clone();
                let name = community
                    .as_ref()
                    .map(|community| community.name.clone())
                    .unwrap_or_else(|| "default".to_owned());
                let id = community.as_ref().map(|community| community.id.clone());
                active_community = id.clone();
                let _ = events
                    .send(ChatEvent::CommunitySwitching {
                        generation,
                        id,
                        name,
                        relay_url,
                    })
                    .await;
                let old_subs = std::mem::replace(&mut subs, sub_tx);
                drop(old_subs);
                tokio::spawn(forward_sub_events(
                    sub_events_rx,
                    events.clone(),
                    generation,
                ));
                spawn_sub_pump(&resolved, sub_rx, sub_events_tx, started_tx);
                let switch_events = events.clone();
                tokio::spawn(async move {
                    if let Ok(Err((_code, reason))) = started_rx.await {
                        let _ = switch_events
                            .send(ChatEvent::Generation {
                                generation,
                                event: Box::new(ChatEvent::Disconnected(reason)),
                            })
                            .await;
                    }
                });
                client = Client::new(&resolved);
            }
            SessionCommand::LoadChannels => {
                load_channels(&client, &subs, &events).await;
            }
            SessionCommand::LoadAgents => {
                load_agents(&client, &events).await;
            }
            SessionCommand::AddAux { channel, ids } => {
                let _ = subs.send(SubControl::AuxAdd { channel, ids }).await;
            }
            SessionCommand::OpenChannel(channel) => {
                if let Some(id) = &active_community
                    && let Err(error) =
                        crate::config::mark_profile_channel(id, &channel.to_string())
                {
                    let _ = events
                        .send(ChatEvent::Status(format!(
                            "cannot record last channel for {id}: {error}"
                        )))
                        .await;
                }
                open_channel(&client, channel, &subs, &events).await;
            }
            SessionCommand::OpenThread {
                channel,
                root,
                request,
            } => {
                open_thread(&client, channel, &root, request, &subs, &events).await;
            }
            SessionCommand::Search { request, token } => {
                search(&client, request, token, &events).await;
            }
            SessionCommand::OpenContext {
                channel,
                target,
                request,
            } => {
                open_context(&client, channel, &target, request, &subs, &events).await;
            }
            SessionCommand::HistoryPage {
                surface,
                channel,
                root,
                request,
                direction,
                cursor,
            } => {
                history_page(
                    &client,
                    PageFetch {
                        surface,
                        channel,
                        root,
                        request,
                        direction,
                        cursor,
                    },
                    &subs,
                    &events,
                )
                .await;
            }
            SessionCommand::NewestWindow {
                surface,
                channel,
                root,
                request,
            } => {
                newest_window(&client, surface, channel, root, request, &subs, &events).await;
            }
            SessionCommand::LoadProfiles(pubkeys) => {
                load_profiles(&client, pubkeys, &events).await;
            }
            SessionCommand::CatchUp(requests) => {
                catch_up(&client, requests, &events).await;
            }
            SessionCommand::ReadProgress { contexts } => {
                // A read is published at most once per window: reading a busy
                // conversation otherwise writes a marker per message, and the
                // newest picture is the only one worth sending anyway.
                pending_read = Some(contexts);
                if due.is_none() {
                    due = Some(tokio::time::Instant::now() + READ_PUBLISH_WINDOW);
                }
            }
            SessionCommand::Send {
                channel,
                content,
                thread,
                local,
                bindings,
            } => {
                // The CLI and this pump share one preflight: a draft that
                // names someone is resolved against the current membership,
                // and a draft that cannot be resolved is not published at all.
                let recipients = match client.plan_mentions(channel, &content, &bindings).await {
                    Ok(recipients) => recipients,
                    Err(block) => {
                        let _ = events
                            .send(ChatEvent::MentionBlocked {
                                local,
                                channel,
                                block,
                            })
                            .await;
                        continue;
                    }
                };
                let outcome = match thread_ref(&thread) {
                    Ok(thread) => {
                        client
                            .send_message(channel, &content, thread, &recipients)
                            .await
                    }
                    Err(failure) => failure.into(),
                };
                report(outcome, local, &events).await;
            }
            SessionCommand::MentionCandidates { channel, request } => {
                // The picker's own read: a failed directory stays visible as a
                // failure, never as an empty member list.
                let result = match client.mention_directory(channel).await {
                    Ok(directory) => Ok(directory.candidates()),
                    Err(failure) => Err(failure.detail),
                };
                let _ = events
                    .send(ChatEvent::MentionCandidates {
                        channel,
                        request,
                        result,
                    })
                    .await;
            }
            SessionCommand::CreateChannel { draft, request } => {
                let outcome = client.create_channel(&draft).await;
                let _ = events
                    .send(ChatEvent::ChannelCreated {
                        request,
                        draft,
                        outcome,
                    })
                    .await;
            }
            SessionCommand::EditChannel {
                channel,
                name,
                description,
                visibility,
                request,
            } => {
                let update = crate::client::ChannelUpdate {
                    name: Some(name),
                    description: Some(description),
                    visibility: Some(visibility),
                };
                let outcome = client.update_channel(channel, &update).await;
                let _ = events
                    .send(ChatEvent::ChannelWriteResult {
                        request,
                        channel,
                        op: ChannelOp::Edit,
                        outcome,
                    })
                    .await;
            }
            SessionCommand::SetArchived {
                channel,
                archived,
                request,
            } => {
                let outcome = if archived {
                    client.archive_channel(channel).await
                } else {
                    client.unarchive_channel(channel).await
                };
                let _ = events
                    .send(ChatEvent::ChannelWriteResult {
                        request,
                        channel,
                        op: if archived {
                            ChannelOp::Archive
                        } else {
                            ChannelOp::Unarchive
                        },
                        outcome,
                    })
                    .await;
            }
            SessionCommand::LoadMembers { channel, request } => {
                let result = client.members(channel).await.map_err(|error| error.detail);
                let _ = events
                    .send(ChatEvent::Members {
                        channel,
                        request,
                        result,
                    })
                    .await;
            }
            SessionCommand::MemberCandidates {
                channel,
                query,
                request,
            } => {
                let result = client
                    .search_profiles(&query)
                    .await
                    .map_err(|error| error.detail);
                let _ = events
                    .send(ChatEvent::MemberCandidates {
                        channel,
                        request,
                        result,
                    })
                    .await;
            }
            SessionCommand::AddMembers {
                channel,
                pubkeys,
                role,
                request,
            } => {
                member_batch = Some(MemberBatch {
                    channel,
                    remaining: pubkeys.into(),
                    role,
                    request,
                });
            }
            SessionCommand::SetMemberRole {
                channel,
                pubkey,
                role,
                request,
            } => {
                let op = MemberOp::SetRole {
                    pubkey: pubkey.clone(),
                    role,
                };
                let outcome = client.set_role(channel, &pubkey, role).await;
                let _ = events
                    .send(ChatEvent::MemberWriteResult {
                        request,
                        channel,
                        op,
                        outcome,
                    })
                    .await;
            }
            SessionCommand::RemoveMember {
                channel,
                pubkey,
                request,
            } => {
                let op = MemberOp::Remove {
                    pubkey: pubkey.clone(),
                };
                let outcome = client.remove_member(channel, &pubkey).await;
                let _ = events
                    .send(ChatEvent::MemberWriteResult {
                        request,
                        channel,
                        op,
                        outcome,
                    })
                    .await;
            }
            SessionCommand::LeaveChannel { channel, request } => {
                let outcome = client.leave_channel(channel).await;
                let _ = events
                    .send(ChatEvent::MemberWriteResult {
                        request,
                        channel,
                        op: MemberOp::Leave,
                        outcome,
                    })
                    .await;
            }
            SessionCommand::Edit {
                channel,
                target,
                content,
                local,
            } => {
                let outcome = match event_id(&target) {
                    Ok(target) => client.edit(channel, target, &content).await,
                    Err(failure) => failure.into(),
                };
                report(outcome, local, &events).await;
            }
            SessionCommand::Delete {
                channel,
                target,
                local,
            } => {
                let outcome = match event_id(&target) {
                    Ok(target) => client.delete(channel, target).await,
                    Err(failure) => failure.into(),
                };
                report(outcome, local, &events).await;
            }
            SessionCommand::React {
                target,
                emoji,
                remove,
                local,
            } => {
                let outcome = match &remove {
                    Some(reaction_id) => match event_id(reaction_id) {
                        Ok(id) => client.remove_reaction(id).await,
                        Err(failure) => failure.into(),
                    },
                    None => match event_id(&target) {
                        Ok(target) => {
                            let emoji = if emoji.is_empty() {
                                content::DEFAULT_REACTION.to_owned()
                            } else {
                                emoji.clone()
                            };
                            client.react(target, &emoji).await
                        }
                        Err(failure) => failure.into(),
                    },
                };
                report(outcome, local, &events).await;
            }
            SessionCommand::Shutdown => {
                flush_read(&client, &mut pending_read, &events).await;
                break;
            }
        }
    }
    let _ = finished.send(());
}

/// Publish a read that is still inside its window. A session that ends must not
/// drop it: the next session would read a marker that never learned about it and
/// show as unread what this one had already shown.
async fn flush_read(
    client: &Client,
    pending: &mut Option<HashMap<String, u64>>,
    events: &mpsc::Sender<ChatEvent>,
) {
    if let Some(contexts) = pending.take() {
        publish_read(client, &contexts, events).await;
    }
}

/// One write result, as the UI renders it. An unconfirmed write is never
/// retried; the row stays marked until the user decides.
async fn report(outcome: WriteOutcome, local: String, events: &mpsc::Sender<ChatEvent>) {
    let event = match outcome {
        WriteOutcome::Stored { event_id } => ChatEvent::WriteOk { local, event_id },
        WriteOutcome::Refused { reason, .. } => ChatEvent::WriteFailed { local, reason },
        WriteOutcome::Unknown { reason, .. } => ChatEvent::WriteUncertain { local, reason },
    };
    let _ = events.send(event).await;
}

/// Fold a catch-up answer into the UI's picture. A seed is a baseline, not
/// unread work; a catch-up is what the conversation holds at or after the
/// frontier. A failed or truncated answer is reported as incomplete, which the
/// UI renders as an unknown rather than as read.
async fn catch_up(
    client: &Client,
    requests: Vec<crate::client::CatchUp>,
    events: &mpsc::Sender<ChatEvent>,
) {
    for answer in client.catch_up(&requests).await {
        let event = if answer.newest {
            ChatEvent::Seed {
                channel: answer.channel,
                latest: answer.events.into_iter().next(),
                complete: answer.complete,
            }
        } else {
            ChatEvent::CatchUp {
                channel: answer.channel,
                events: answer.events,
                complete: answer.complete,
            }
        };
        let _ = events.send(event).await;
    }
}

/// Publish what this terminal has read. The client reads the remote state back
/// and merges before writing, so a second terminal's progress is not
/// overwritten; a failed read-back is a failed publish rather than a blind
/// write, and the local view keeps saying it is not synced.
async fn publish_read(
    client: &Client,
    contexts: &HashMap<String, u64>,
    events: &mpsc::Sender<ChatEvent>,
) {
    match client.publish_read_state(contexts).await {
        Ok(merged) => {
            let _ = events
                .send(ChatEvent::ReadPublished {
                    ok: true,
                    reason: String::new(),
                })
                .await;
            // The read-back is a fresh answer as well: a conversation whose
            // marker could not be read before is known now.
            let _ = events
                .send(ChatEvent::ReadState {
                    contexts: merged,
                    complete: true,
                })
                .await;
        }
        Err(reason) => {
            let _ = events
                .send(ChatEvent::ReadPublished { ok: false, reason })
                .await;
        }
    }
}

/// The identity's read frontier, as the relay holds it. A failed lookup is
/// unknown, not absence: the UI is told the answer is incomplete and refuses to
/// treat a missing marker as read.
async fn load_read_state(client: &Client, events: &mpsc::Sender<ChatEvent>) {
    match client.read_state().await {
        Ok(state) => {
            let _ = events
                .send(ChatEvent::ReadState {
                    contexts: state.contexts,
                    complete: !state.gaps,
                })
                .await;
            if state.gaps {
                let _ = events
                    .send(ChatEvent::Status(
                        "a read-state slot could not be decoded".to_owned(),
                    ))
                    .await;
            }
        }
        Err(failure) => {
            let _ = events
                .send(ChatEvent::ReadState {
                    contexts: HashMap::new(),
                    complete: false,
                })
                .await;
            let _ = events
                .send(ChatEvent::Status(format!("read state unknown: {failure}")))
                .await;
        }
    }
}

async fn load_channels(
    client: &Client,
    subs: &mpsc::Sender<SubControl>,
    events: &mpsc::Sender<ChatEvent>,
) {
    let roster = match client.channels().await {
        Ok(roster) => roster,
        Err(failure) => {
            let _ = events
                .send(ChatEvent::Status(format!("channel list failed: {failure}")))
                .await;
            return;
        }
    };
    // Every listed conversation, not just the open one: the channel list shows
    // an activity marker per conversation, and an indicator that arrives while
    // the user is elsewhere is exactly what the marker is for. A row the list
    // leaves out is not subscribed to.
    let ids: Vec<Uuid> = roster
        .items
        .iter()
        .filter(|channel| channel.listed())
        .map(|channel| channel.id)
        .collect();
    let _ = subs.send(SubControl::Typing(ids.clone())).await;
    let _ = subs.send(SubControl::Inbox(ids)).await;
    let _ = events.send(ChatEvent::Channels(roster)).await;
    // The read markers, after the roster: the UI needs the list before it can
    // say which conversation a frontier belongs to, and it asks for its
    // catch-up once this answer lands.
    load_read_state(client, events).await;
}

/// Read the owned roster. A refusal is its own answer: an identity that may not
/// read an owner roster must not be shown an empty one.
async fn load_agents(client: &Client, events: &mpsc::Sender<ChatEvent>) {
    let load = match client.managed_agents().await {
        Ok(roster) => crate::agents::Load::Loaded(roster),
        Err(failure) if failure.category == Category::Forbidden => {
            crate::agents::Load::Unavailable(failure.detail)
        }
        Err(failure) => crate::agents::Load::Failed(failure.detail),
    };
    let _ = events.send(ChatEvent::Agents(load)).await;
}

async fn open_channel(
    client: &Client,
    channel: Uuid,
    subs: &mpsc::Sender<SubControl>,
    events: &mpsc::Sender<ChatEvent>,
) {
    let _ = subs.send(SubControl::Timeline(channel)).await;
    let history = match client.history(channel, HISTORY_LIMIT).await {
        Ok(history) => history,
        Err(failure) => {
            let _ = events
                .send(ChatEvent::HistoryFailed {
                    channel,
                    reason: failure.to_string(),
                })
                .await;
            return;
        }
    };
    let ids: Vec<String> = history.iter().map(|event| event.id.to_hex()).collect();
    // Install the HTTP rows before the aux query can deliver an overlay. The
    // live feed is already active, so this ordering closes the initial
    // history/aux race as well as the later reload race.
    let _ = events
        .send(ChatEvent::History {
            channel,
            events: history,
        })
        .await;
    let _ = subs.send(SubControl::Aux { channel, ids }).await;
}

/// Read one bounded thread: the root and up to the reply bound. The rows are
/// installed before the aux feed is extended, so the overlays of a thread that
/// is already on screen cannot land before the rows they belong to.
async fn open_thread(
    client: &Client,
    channel: Uuid,
    root: &str,
    request: u64,
    subs: &mpsc::Sender<SubControl>,
    events: &mpsc::Sender<ChatEvent>,
) {
    let root = root.to_owned();
    let id = match event_id(&root) {
        Ok(id) => id,
        Err(failure) => {
            let _ = events
                .send(ChatEvent::ThreadFailed {
                    channel,
                    root,
                    request,
                    reason: failure.to_string(),
                })
                .await;
            return;
        }
    };
    let read = match client.thread(id, Some(channel)).await {
        Ok(read) => read,
        Err(failure) => {
            let _ = events
                .send(ChatEvent::ThreadFailed {
                    channel,
                    root,
                    request,
                    reason: failure.to_string(),
                })
                .await;
            return;
        }
    };
    let ids: Vec<String> = read.events.iter().map(|event| event.id.to_hex()).collect();
    let _ = events
        .send(ChatEvent::Thread {
            channel,
            root,
            request,
            events: read.events,
            partial: read.partial,
        })
        .await;
    if !ids.is_empty() {
        let _ = subs.send(SubControl::AuxAdd { channel, ids }).await;
    }
}
async fn search(
    client: &Client,
    request: crate::client::SearchRequest,
    token: u64,
    events: &mpsc::Sender<ChatEvent>,
) {
    let result = client.search(&request).await;
    match result {
        Ok(found) => {
            let _ = events
                .send(ChatEvent::Search {
                    request: token,
                    events: found,
                })
                .await;
        }
        Err(failure) => {
            let _ = events
                .send(ChatEvent::SearchFailed {
                    request: token,
                    reason: failure.to_string(),
                })
                .await;
        }
    }
}

async fn open_context(
    client: &Client,
    channel: Uuid,
    target: &str,
    request: u64,
    subs: &mpsc::Sender<SubControl>,
    events: &mpsc::Sender<ChatEvent>,
) {
    let target_owned = target.to_owned();
    let id = match event_id(target) {
        Ok(id) => id,
        Err(failure) => {
            let _ = events
                .send(ChatEvent::ContextFailed {
                    channel,
                    target: target_owned,
                    request,
                    reason: failure.to_string(),
                })
                .await;
            return;
        }
    };
    match client.context(id, channel, 20).await {
        Ok(read) => {
            let ids: Vec<String> = read.events.iter().map(|event| event.id.to_hex()).collect();
            let _ = events
                .send(ChatEvent::Context {
                    channel,
                    target: target_owned,
                    request,
                    events: read.events,
                    before_complete: read.before_complete,
                    after_complete: read.after_complete,
                })
                .await;
            if !ids.is_empty() {
                let _ = subs.send(SubControl::AuxAdd { channel, ids }).await;
            }
        }
        Err(failure) => {
            let _ = events
                .send(ChatEvent::ContextFailed {
                    channel,
                    target: target_owned,
                    request,
                    reason: failure.to_string(),
                })
                .await;
        }
    }
}

/// One page request, grouped the way the fetch reads it.
struct PageFetch {
    surface: HistorySurface,
    channel: Uuid,
    root: Option<String>,
    request: u64,
    direction: crate::client::HistoryDirection,
    cursor: u64,
}

async fn history_page(
    client: &Client,
    fetch: PageFetch,
    subs: &mpsc::Sender<SubControl>,
    events: &mpsc::Sender<ChatEvent>,
) {
    let PageFetch {
        surface,
        channel,
        root,
        request,
        direction,
        cursor,
    } = fetch;
    let root = root.as_deref();
    let result = match (surface, root) {
        (HistorySurface::Thread, Some(root)) => match event_id(root) {
            Ok(root) => client
                .thread_page(root, channel, direction, cursor, 50)
                .await
                .map_err(|failure| failure.to_string()),
            Err(failure) => Err(failure.to_string()),
        },
        (HistorySurface::Thread, None) => Err("thread root is missing".to_owned()),
        _ => client
            .history_page(channel, direction, cursor, 50)
            .await
            .map_err(|failure| failure.to_string()),
    };
    match result {
        Ok(page) => {
            let ids: Vec<String> = page.events.iter().map(|event| event.id.to_hex()).collect();
            let _ = events
                .send(ChatEvent::HistoryPage {
                    surface,
                    channel,
                    root: root.map(str::to_owned),
                    request,
                    direction,
                    events: page.events,
                    saturated: page.saturated,
                })
                .await;
            if !ids.is_empty() {
                let _ = subs.send(SubControl::AuxAdd { channel, ids }).await;
            }
        }
        Err(reason) => {
            let _ = events
                .send(ChatEvent::HistoryPageFailed {
                    surface,
                    channel,
                    root: root.map(str::to_owned),
                    request,
                    direction,
                    reason,
                })
                .await;
        }
    }
}

/// The newest window of one surface, behind `G`. The thread surface reads its
/// root plus the newest reply bound; a channel or context surface reads the
/// conversation's newest timeline window. Rows go out before the aux query,
/// the same ordering an open keeps, so an overlay cannot land before the row
/// it belongs to.
async fn newest_window(
    client: &Client,
    surface: HistorySurface,
    channel: Uuid,
    root: Option<String>,
    request: u64,
    subs: &mpsc::Sender<SubControl>,
    events: &mpsc::Sender<ChatEvent>,
) {
    let root_ref = root.as_deref();
    let result = match (surface, root_ref) {
        (HistorySurface::Thread, Some(root)) => match event_id(root) {
            Ok(id) => client
                .thread(id, Some(channel))
                .await
                .map(|read| crate::client::HistoryPage {
                    events: read.events,
                    saturated: read.partial,
                })
                .map_err(|failure| failure.to_string()),
            Err(failure) => Err(failure.to_string()),
        },
        (HistorySurface::Thread, None) => Err("thread root is missing".to_owned()),
        _ => client
            .latest(channel, HISTORY_LIMIT)
            .await
            .map_err(|failure| failure.to_string()),
    };
    match result {
        Ok(page) => {
            let ids: Vec<String> = page.events.iter().map(|event| event.id.to_hex()).collect();
            let _ = events
                .send(ChatEvent::NewestWindow {
                    surface,
                    channel,
                    root: root_ref.map(str::to_owned),
                    request,
                    events: page.events,
                    saturated: page.saturated,
                })
                .await;
            if !ids.is_empty() {
                let _ = subs.send(SubControl::AuxAdd { channel, ids }).await;
            }
        }
        Err(reason) => {
            let _ = events
                .send(ChatEvent::NewestWindowFailed {
                    surface,
                    channel,
                    root: root_ref.map(str::to_owned),
                    request,
                    reason,
                })
                .await;
        }
    }
}

async fn load_profiles(client: &Client, pubkeys: Vec<String>, events: &mpsc::Sender<ChatEvent>) {
    // A name that does not resolve stays a short key, so a failed read is
    // not worth a status line.
    let Ok(resolved) = client.profiles(pubkeys).await else {
        return;
    };
    if !resolved.is_empty() {
        let _ = events.send(ChatEvent::Profiles(resolved)).await;
    }
}

/// `buzzx watch`: stream one channel's live events as JSON lines. The exit
/// codes are the shared ones: 2 unreachable, 3 auth refused.
pub async fn run_watch(resolved: &Resolved, channel: Uuid) -> i32 {
    let (events_tx, mut events_rx) = mpsc::channel(256);
    let (sub_tx, sub_rx) = mpsc::channel(8);
    let (started_tx, started_rx) = oneshot::channel();
    let (_http_url, ws_url) =
        crate::config::split_relay_url(&resolved.http_url).expect("a resolved URL always splits");
    let auth_tag = resolved.auth_tag.as_deref().map(|s| {
        buzz_sdk::nip_oa::parse_auth_tag(s).expect("resolve() verified the tag at startup")
    });
    tokio::spawn(sub::run_ws_pump(
        ws_url,
        resolved.keys.clone(),
        auth_tag,
        sub_rx,
        events_tx,
        started_tx,
    ));
    match started_rx.await {
        Ok(Ok(())) => {}
        Ok(Err((code, reason))) => {
            eprintln!("buzzx: {reason}");
            return code;
        }
        Err(_) => return crate::config::EXIT_OTHER,
    }
    if sub_tx.send(SubControl::Timeline(channel)).await.is_err() {
        return crate::config::EXIT_OTHER;
    }
    loop {
        tokio::select! {
            event = events_rx.recv() => match event {
                Some(ChatEvent::Timeline { event, .. }) => println!("{}", serde_json::to_string(&event).unwrap_or_default()),
                Some(ChatEvent::Status(reason)) => eprintln!("buzzx: {reason}"),
                Some(_) => {}
                None => return crate::config::EXIT_OTHER,
            },
            _ = tokio::signal::ctrl_c() => return 0,
        }
    }
}
