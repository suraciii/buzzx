//! The session: the only module that touches both transports. It owns the
//! command pump (HTTP reads and writes) and spawns the WebSocket pump. The
//! UI never awaits a network call; it sends a `SessionCommand` and keeps
//! rendering until a `ChatEvent` arrives.

use std::collections::HashSet;

use buzz_sdk::builders::{
    build_delete_message, build_edit, build_message, build_reaction, build_remove_reaction,
};
use nostr::{Event, EventId, Keys, Tag};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::config::Resolved;
use crate::content::{self, MEMBERSHIP_KIND, PROFILE_KIND};
use crate::sub::{self, SubControl};

/// The tag that names a channel id on membership and metadata events.
const D_TAG: &str = "d";
/// The tag that names a channel's display name on metadata events.
const NAME_TAG: &str = "name";
use crate::http::{HttpTransport, WriteOutcome};
/// How many channel ids one membership or metadata query carries.
const CHANNEL_QUERY_LIMIT: u64 = 500;
/// How many history events one open fetches.
const HISTORY_LIMIT: u64 = 100;
/// How many authors one profile query carries.
const PROFILE_CHUNK: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelInfo {
    pub id: Uuid,
    pub name: String,
}

/// What the transport side tells the UI. Every mutation the UI renders
/// arrives as one of these.
#[derive(Debug)]
pub enum ChatEvent {
    Connected,
    Disconnected(String),
    /// The identity's channel list. Sent after connect and after a reload.
    Channels(Vec<ChannelInfo>),
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
    /// live connection only and is never part of the timeline.
    Typing {
        channel: Uuid,
        pubkey: String,
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
        auth_tag.clone(),
        sub_rx,
        events.clone(),
        started_tx,
    ));

    let transport = HttpTransport::new(
        &resolved.http_url,
        resolved.keys.clone(),
        resolved.auth_tag.clone(),
    );
    tokio::spawn(run_command_pump(
        transport,
        resolved.keys.clone(),
        auth_tag,
        cmd_rx,
        sub_tx,
        events,
    ));

    Session {
        commands: cmd_tx,
        started: started_rx,
    }
}

fn thread_ref(thread: &Option<(String, String)>) -> Option<buzz_sdk::ThreadRef> {
    let (root, parent) = thread.as_ref()?;
    Some(buzz_sdk::ThreadRef {
        root_event_id: EventId::from_hex(root).ok()?,
        parent_event_id: EventId::from_hex(parent).ok()?,
    })
}

/// Sign a builder with the identity, attaching the NIP-OA tag when the
/// identity carries one. The event tag is authoritative on the relay.
fn sign(
    builder: nostr::EventBuilder,
    keys: &Keys,
    auth_tag: &Option<Tag>,
) -> Result<Event, String> {
    let builder = match auth_tag {
        Some(tag) => builder.tag(tag.clone()),
        None => builder,
    };
    builder
        .sign_with_keys(keys)
        .map_err(|e| format!("signing failed: {e}"))
}

async fn run_command_pump(
    transport: HttpTransport,
    keys: Keys,
    auth_tag: Option<Tag>,
    mut commands: mpsc::Receiver<SessionCommand>,
    subs: mpsc::Sender<SubControl>,
    events: mpsc::Sender<ChatEvent>,
) {
    let me = keys.public_key().to_hex();
    while let Some(command) = commands.recv().await {
        match command {
            SessionCommand::LoadChannels => {
                load_channels(&transport, &me, &subs, &events).await;
            }
            SessionCommand::OpenChannel(channel) => {
                open_channel(&transport, channel, &subs, &events).await;
            }
            SessionCommand::LoadProfiles(pubkeys) => {
                load_profiles(&transport, pubkeys, &events).await;
            }
            SessionCommand::Send {
                channel,
                content,
                thread,
                local,
            } => {
                let builder = build_message(
                    channel,
                    &content,
                    thread_ref(&thread).as_ref(),
                    &[],
                    false,
                    &[],
                );
                let outcome = match builder {
                    Ok(builder) => {
                        submit(builder, &transport, &keys, &auth_tag, &local, &events).await
                    }
                    Err(e) => Err(e.to_string()),
                };
                report_refusal(outcome, local, &events).await;
            }
            SessionCommand::Edit {
                channel,
                target,
                content,
                local,
            } => {
                let builder = match EventId::from_hex(&target) {
                    Ok(target) => build_edit(channel, target, &content).map_err(|e| e.to_string()),
                    Err(_) => Err("edit target is not an event id".to_owned()),
                };
                let outcome = match builder {
                    Ok(builder) => {
                        submit(builder, &transport, &keys, &auth_tag, &local, &events).await
                    }
                    Err(e) => Err(e.to_string()),
                };
                report_refusal(outcome, local, &events).await;
            }
            SessionCommand::Delete {
                channel,
                target,
                local,
            } => {
                let builder = match EventId::from_hex(&target) {
                    Ok(target) => build_delete_message(channel, target).map_err(|e| e.to_string()),
                    Err(_) => Err("delete target is not an event id".to_owned()),
                };
                let outcome = match builder {
                    Ok(builder) => {
                        submit(builder, &transport, &keys, &auth_tag, &local, &events).await
                    }
                    Err(e) => Err(e.to_string()),
                };
                report_refusal(outcome, local, &events).await;
            }
            SessionCommand::React {
                target,
                emoji,
                remove,
                local,
            } => {
                let builder = match (&remove, EventId::from_hex(&target)) {
                    (Some(reaction_id), _) => match EventId::from_hex(reaction_id) {
                        Ok(id) => build_remove_reaction(id).map_err(|e| e.to_string()),
                        Err(e) => Err(format!("reaction id is invalid: {e}")),
                    },
                    (None, Ok(target)) => {
                        let emoji = if emoji.is_empty() {
                            content::DEFAULT_REACTION.to_owned()
                        } else {
                            emoji.clone()
                        };
                        build_reaction(target, &emoji).map_err(|e| e.to_string())
                    }
                    (None, Err(_)) => Err("react target is not an event id".to_owned()),
                };
                let outcome = match builder {
                    Ok(builder) => {
                        submit(builder, &transport, &keys, &auth_tag, &local, &events).await
                    }
                    Err(e) => Err(e.to_string()),
                };
                report_refusal(outcome, local, &events).await;
            }
            SessionCommand::Shutdown => return,
        }
    }
}

async fn submit(
    builder: nostr::EventBuilder,
    transport: &HttpTransport,
    keys: &Keys,
    auth_tag: &Option<Tag>,
    local: &str,
    events: &mpsc::Sender<ChatEvent>,
) -> Result<(), String> {
    let event = match sign(builder, keys, auth_tag) {
        Ok(event) => event,
        Err(e) => return Err(e),
    };
    match transport.submit(&event).await {
        WriteOutcome::Ok { event_id } => {
            // An empty stored id falls back to the signed id; a body without
            // one is not a refusal.
            let event_id = if event_id.is_empty() {
                event.id.to_hex()
            } else {
                event_id
            };
            let _ = events
                .send(ChatEvent::WriteOk {
                    local: local.to_owned(),
                    event_id,
                })
                .await;
            Ok(())
        }
        WriteOutcome::Failed { reason } => Err(reason),
        WriteOutcome::Uncertain { reason } => {
            let _ = events
                .send(ChatEvent::WriteUncertain {
                    local: local.to_owned(),
                    reason,
                })
                .await;
            Ok(())
        }
    }
}

/// A build or sign failure is a refusal the UI must show; the composer draft
/// is restored for it just like a relay refusal.
async fn report_refusal(
    outcome: Result<(), String>,
    local: String,
    events: &mpsc::Sender<ChatEvent>,
) {
    if let Err(reason) = outcome {
        let _ = events.send(ChatEvent::WriteFailed { local, reason }).await;
    }
}

fn tag_value(event: &Event, name: &str) -> Option<String> {
    event.tags.iter().find_map(|t| {
        let parts = t.as_slice();
        (parts.first().map(String::as_str) == Some(name))
            .then(|| parts.get(1).cloned())
            .flatten()
    })
}

async fn load_channels(
    transport: &HttpTransport,
    me: &str,
    subs: &mpsc::Sender<SubControl>,
    events: &mpsc::Sender<ChatEvent>,
) {
    // Membership first: kind 39002 names the channels the identity belongs
    // to. Metadata for those ids second.
    let membership = serde_json::json!({
        "kinds": [MEMBERSHIP_KIND],
        "#p": [me],
        "limit": CHANNEL_QUERY_LIMIT,
    });
    let roster = match transport.query(&membership).await {
        Ok(events) => events,
        Err(e) => {
            let _ = events
                .send(ChatEvent::Status(format!("channel list failed: {e}")))
                .await;
            return;
        }
    };
    let mut ids: Vec<String> = Vec::new();
    for event in &roster {
        if let Some(id) = tag_value(event, D_TAG).filter(|id| !id.is_empty() && !ids.contains(id)) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        // No membership means no channel to watch for typing either; the
        // empty list replaces whatever the previous connection subscribed to.
        let _ = subs.send(SubControl::Typing(Vec::new())).await;
        let _ = events.send(ChatEvent::Channels(Vec::new())).await;
        return;
    }
    let metadata = serde_json::json!({
        "kinds": [content::CHANNEL_METADATA_KIND],
        "#d": ids,
        "limit": CHANNEL_QUERY_LIMIT,
    });
    let described = transport.query(&metadata).await.unwrap_or_default();
    let mut channels: Vec<ChannelInfo> = Vec::new();
    for event in &described {
        let Some(id) = tag_value(event, D_TAG).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Ok(id) = Uuid::parse_str(&id) else {
            continue;
        };
        let name = tag_value(event, NAME_TAG)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| id.to_string());
        channels.push(ChannelInfo { id, name });
    }
    channels.sort_by_key(|c| c.name.to_lowercase());
    // Every member channel, not just the open one: the channel list shows an
    // activity marker per channel, and an indicator that arrives while the
    // user is elsewhere is exactly what the marker is for.
    let ids: Vec<Uuid> = channels.iter().map(|c| c.id).collect();
    let _ = subs.send(SubControl::Typing(ids)).await;
    let _ = events.send(ChatEvent::Channels(channels)).await;
}

async fn open_channel(
    transport: &HttpTransport,
    channel: Uuid,
    subs: &mpsc::Sender<SubControl>,
    events: &mpsc::Sender<ChatEvent>,
) {
    let _ = subs.send(SubControl::Timeline(channel)).await;
    let filter = serde_json::json!({
        "kinds": content::TIMELINE_KINDS,
        "#h": [channel.to_string()],
        "limit": HISTORY_LIMIT,
    });
    let mut history = match transport.query(&filter).await {
        Ok(events) => events,
        Err(e) => {
            let _ = events
                .send(ChatEvent::Status(format!("history failed: {e}")))
                .await;
            return;
        }
    };
    // The relay returns newest first; the timeline renders oldest first.
    history.sort_by_key(|e| e.created_at.as_secs());

    let ids: Vec<String> = history.iter().map(|e| e.id.to_hex()).collect();
    let _ = subs.send(SubControl::Aux { channel, ids }).await;
    let _ = events
        .send(ChatEvent::History {
            channel,
            events: history,
        })
        .await;
}

async fn load_profiles(
    transport: &HttpTransport,
    pubkeys: Vec<String>,
    events: &mpsc::Sender<ChatEvent>,
) {
    let mut seen: HashSet<String> = HashSet::new();
    let wanted: Vec<String> = pubkeys
        .into_iter()
        .filter(|k| seen.insert(k.clone()))
        .take(200)
        .collect();
    for chunk in wanted.chunks(PROFILE_CHUNK) {
        let filter = serde_json::json!({
            "kinds": [PROFILE_KIND],
            "authors": chunk,
            "limit": chunk.len() as u64,
        });
        let Ok(found) = transport.query(&filter).await else {
            continue;
        };
        let mut resolved: Vec<(String, String)> = Vec::new();
        for event in found {
            if let Some((name, display)) = content::profile_names(&event) {
                let best = display.or(name);
                if let Some(best) = best {
                    resolved.push((event.pubkey.to_hex(), best));
                }
            }
        }
        if !resolved.is_empty() {
            let _ = events.send(ChatEvent::Profiles(resolved)).await;
        }
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
