//! The live session: the subscriptions, the reconnect, and the `ChatEvent`
//! stream the TUI renders. The command pump translates one `SessionCommand`
//! into one core call and one `ChatEvent`; the relay operations themselves
//! live in `client.rs`. The contract is design/shared-core.md.

use nostr::Event;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::client::{Client, WriteOutcome, event_id};
use crate::config::Resolved;
use crate::content;
use crate::failure::Failure;
use crate::sub::{self, SubControl};

/// How many history events one open fetches.
const HISTORY_LIMIT: u64 = 100;

/// What the transport side tells the UI. Every mutation the UI renders
/// arrives as one of these.
#[derive(Debug)]
pub enum ChatEvent {
    Connected,
    Disconnected(String),
    /// The identity's channel list. Sent after connect and after a reload.
    Channels(Vec<crate::client::ChannelInfo>),
    /// A channel's history, oldest first. Replaces the loaded rows.
    History {
        channel: Uuid,
        events: Vec<Event>,
    },
    /// Display names for authors the UI has seen.
    Profiles(Vec<(String, String)>),
    /// One live timeline event for a subscribed channel.
    Timeline {
        channel: Uuid,
        event: Event,
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
    /// A relay notice or a publish answer worth showing on the status line.
    Status(String),
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
    /// Fetch history, subscribe live, and backfill overlays for one channel.
    OpenChannel(Uuid),
    /// Resolve display names for authors the UI is missing.
    LoadProfiles(Vec<String>),
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
}

/// Start the session: one WebSocket pump and one command pump, both feeding
/// the same event channel.
pub fn spawn(resolved: &Resolved, events: mpsc::Sender<ChatEvent>) -> Session {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (sub_tx, sub_rx) = mpsc::channel(64);
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
        events.clone(),
        started_tx,
    ));

    tokio::spawn(run_command_pump(
        Client::new(resolved),
        cmd_rx,
        sub_tx,
        events,
    ));

    Session {
        commands: cmd_tx,
        started: started_rx,
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
) {
    while let Some(command) = commands.recv().await {
        match command {
            SessionCommand::LoadChannels => {
                load_channels(&client, &subs, &events).await;
            }
            SessionCommand::OpenChannel(channel) => {
                open_channel(&client, channel, &subs, &events).await;
            }
            SessionCommand::LoadProfiles(pubkeys) => {
                load_profiles(&client, pubkeys, &events).await;
            }
            SessionCommand::Send {
                channel,
                content,
                thread,
                local,
            } => {
                let outcome = match thread_ref(&thread) {
                    Ok(thread) => client.send_message(channel, &content, thread).await,
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
            SessionCommand::Shutdown => return,
        }
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

async fn load_channels(
    client: &Client,
    subs: &mpsc::Sender<SubControl>,
    events: &mpsc::Sender<ChatEvent>,
) {
    let channels = match client.channels().await {
        Ok(channels) => channels,
        Err(failure) => {
            let _ = events
                .send(ChatEvent::Status(format!("channel list failed: {failure}")))
                .await;
            return;
        }
    };
    // Every member channel, not just the open one: the channel list shows an
    // activity marker per channel, and an indicator that arrives while the
    // user is elsewhere is exactly what the marker is for.
    let ids: Vec<Uuid> = channels.iter().map(|channel| channel.id).collect();
    let _ = subs.send(SubControl::Typing(ids)).await;
    let _ = events.send(ChatEvent::Channels(channels)).await;
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
                .send(ChatEvent::Status(format!("history failed: {failure}")))
                .await;
            return;
        }
    };
    let ids: Vec<String> = history.iter().map(|event| event.id.to_hex()).collect();
    let _ = subs.send(SubControl::Aux { channel, ids }).await;
    let _ = events
        .send(ChatEvent::History {
            channel,
            events: history,
        })
        .await;
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
