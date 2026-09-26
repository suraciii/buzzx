//! The live session: the subscriptions, the reconnect, and the `ChatEvent`
//! stream the TUI renders. The command pump translates one `SessionCommand`
//! into one core call and one `ChatEvent`; the relay operations themselves
//! live in `client.rs`. The contract is design/shared-core.md.

use std::collections::HashMap;

use nostr::Event;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::client::{Client, WriteOutcome, event_id};
use crate::config::Resolved;
use crate::content;
use crate::failure::{Category, Failure};
use crate::mentions;
use crate::sub::{self, SubControl};

/// How many history events one open fetches.
const HISTORY_LIMIT: u64 = 100;
/// How long a read waits before it is published. Reading a busy conversation
/// otherwise writes a marker per message; the newest picture is the one worth
/// sending.
const READ_PUBLISH_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// What the transport side tells the UI. Every mutation the UI renders
/// arrives as one of these.
#[derive(Debug)]
pub enum ChatEvent {
    Connected,
    Disconnected(String),
    /// The identity's channel list. Sent after connect and after a reload.
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
    /// `summary` is the status line; `details` are the exact references the
    /// scrollable help surface shows.
    MentionBlocked {
        local: String,
        summary: String,
        details: Vec<String>,
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
}

/// What the UI asks the session to do.
#[derive(Debug)]
pub enum SessionCommand {
    LoadChannels,
    /// Read the managed-agent roster this identity owns. Asked for when the
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
    /// a quitting session. A process that exits on its own must wait for this,
    /// or the last write is cut off mid-flight.
    pub finished: oneshot::Receiver<()>,
}

/// Start the session: one WebSocket pump and one command pump, both feeding
/// the same event channel.
pub fn spawn(resolved: &Resolved, events: mpsc::Sender<ChatEvent>) -> Session {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (sub_tx, sub_rx) = mpsc::channel(64);
    let (started_tx, started_rx) = oneshot::channel();
    let (finished_tx, finished_rx) = oneshot::channel();

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
        events.clone(),
        started_tx,
    ));

    tokio::spawn(run_command_pump(
        Client::new(resolved),
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

async fn run_command_pump(
    client: Client,
    mut commands: mpsc::Receiver<SessionCommand>,
    subs: mpsc::Sender<SubControl>,
    events: mpsc::Sender<ChatEvent>,
    finished: oneshot::Sender<()>,
) {
    // What this terminal has read, waiting for its window to close.
    let mut pending_read: Option<HashMap<String, u64>> = None;
    let mut due: Option<tokio::time::Instant> = None;
    loop {
        let command = match due {
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
        };
        let Some(command) = command else {
            // The command channel closing is the other way a session ends, and
            // it owes the same flush as a shutdown command.
            flush_read(&client, &mut pending_read, &events).await;
            break;
        };
        match command {
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
                open_channel(&client, channel, &subs, &events).await;
            }
            SessionCommand::OpenThread {
                channel,
                root,
                request,
            } => {
                open_thread(&client, channel, &root, request, &subs, &events).await;
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
            } => {
                // A draft with no mention input is sent without reading the
                // roster; one that names someone is resolved against the
                // current membership first, and a draft that cannot be
                // resolved is not published at all.
                let planned = if mentions::needs_lookup(&content) {
                    match client.mention_directory(channel).await {
                        Ok(directory) => mentions::plan(&content, &directory),
                        Err(failure) => Err(mentions::Block::LookupFailed {
                            reason: failure.detail,
                        }),
                    }
                } else {
                    Ok(Vec::new())
                };
                let recipients = match planned {
                    Ok(recipients) => recipients,
                    Err(block) => {
                        let _ = events
                            .send(ChatEvent::MentionBlocked {
                                local,
                                summary: block.summary(),
                                details: block.details(),
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
